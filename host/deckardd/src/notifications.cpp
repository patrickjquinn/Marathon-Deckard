#include "notifications.h"

#include "bridge.h"
#include "images.h"
#include "validate.h"

#include <QCryptographicHash>
#include <QDBusArgument>
#include <QDBusConnection>
#include <QDBusMessage>
#include <QDBusMetaType>
#include <QDBusPendingCall>
#include <QDataStream>
#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QLoggingCategory>
#include <QSaveFile>
#include <QDir>
#include <QTimer>

Q_LOGGING_CATEGORY(lcNotify, "deckard.notifications")

namespace {
    const QString kService   = QStringLiteral("org.freedesktop.Notifications");
    const QString kPath      = QStringLiteral("/org/freedesktop/Notifications");
    const QString kInterface = QStringLiteral("org.freedesktop.Notifications");

    struct ImageData {
        int        width = 0, height = 0, rowstride = 0;
        bool       alpha = true;
        int        bits = 8, channels = 4;
        QByteArray data;
    };
} // namespace

Q_DECLARE_METATYPE(ImageData)

QDBusArgument &operator<<(QDBusArgument &arg, const ImageData &image) {
    arg.beginStructure();
    arg << image.width << image.height << image.rowstride << image.alpha << image.bits
        << image.channels << image.data;
    arg.endStructure();
    return arg;
}

const QDBusArgument &operator>>(const QDBusArgument &arg, ImageData &image) {
    arg.beginStructure();
    arg >> image.width >> image.height >> image.rowstride >> image.alpha >> image.bits >>
        image.channels >> image.data;
    arg.endStructure();
    return arg;
}

Notifications::Notifications(Bridge *bridge, const QString &iconDir, const QString &stateFile,
                             QObject *parent)
    : QObject(parent)
    , m_bridge(bridge)
    , m_iconDir(iconDir)
    , m_stateFile(stateFile) {
    qDBusRegisterMetaType<ImageData>();
    QDBusConnection bus = QDBusConnection::sessionBus();
    bus.connect(kService, kPath, kInterface, QStringLiteral("ActionInvoked"), this,
                SLOT(onActionInvoked(uint, QString)));
    bus.connect(kService, kPath, kInterface, QStringLiteral("NotificationClosed"), this,
                SLOT(onNotificationClosed(uint, uint)));
    bus.connect(kService, kPath, kInterface, QStringLiteral("NotificationReplied"), this,
                SLOT(onNotificationReplied(uint, QString)));
    load();
}

void Notifications::handle(const QString &type, const QJsonObject &message) {
    if (type == QLatin1String("post"))
        post(message);
    else if (type == QLatin1String("remove"))
        remove(message.value(QStringLiteral("key")).toString());
    else if (type == QLatin1String("sync"))
        sync(message);
    else if (type == QLatin1String("icon"))
        icon(message);
}

void Notifications::post(const QJsonObject &message) {
    QString                            error;
    const std::optional<Validate::Post> p = Validate::post(message, &error);
    if (!p) {
        qCWarning(lcNotify) << "rejected a notification:" << error;
        return;
    }
    if (!m_entries.contains(p->key) && m_entries.size() >= kMaxTracked) {
        qCWarning(lcNotify) << "too many notifications; dropped one from" << p->pkg;
        return;
    }

    QVariantMap hints{
        {QStringLiteral("desktop-entry"), QString(QStringLiteral("waydroid.") + p->pkg)},
        {QStringLiteral("urgency"), QVariant::fromValue(uchar(p->urgency))},
        {QStringLiteral("resident"), p->ongoing},
        {QStringLiteral("suppress-sound"), p->silent}};
    if (!p->category.isEmpty())
        hints.insert(QStringLiteral("category"), p->category);
    if (!p->replyAction.isEmpty() && !p->replyPlaceholder.isEmpty())
        hints.insert(QStringLiteral("x-kde-reply-placeholder-text"), p->replyPlaceholder);
    if (!p->conversation.isEmpty())
        hints.insert(QStringLiteral("x-marathon-conversation"), p->conversation);
    if (p->progress != Validate::kNoProgress)
        hints.insert(QStringLiteral("value"), p->progress);

    QCryptographicHash digest(QCryptographicHash::Sha256);
    {
        QByteArray  bytes;
        QDataStream stream(&bytes, QIODevice::WriteOnly);
        QVariantMap shown = hints;
        shown.remove(QStringLiteral("suppress-sound"));
        stream << p->label << p->title << p->body << p->actions << shown << p->image;
        digest.addData(bytes);
    }
    Entry entry = m_entries.value(p->key);
    if (entry.id && entry.digest == digest.result())
        return;

    const QImage image = Images::decode(p->image, 128);
    if (!image.isNull()) {
        hints.insert(QStringLiteral("image-data"),
                     QVariant::fromValue(ImageData{image.width(), image.height(),
                                                   int(image.bytesPerLine()), true, 8, 4,
                                                   QByteArray(reinterpret_cast<const char *>(
                                                                  image.constBits()),
                                                              image.sizeInBytes())}));
    }
    const QString iconPath = m_iconDir + QLatin1Char('/') + p->pkg + QStringLiteral(".png");
    const QString appIcon =
        QFileInfo::exists(iconPath) ? QStringLiteral("file://") + iconPath : QString();

    QDBusMessage call = QDBusMessage::createMethodCall(kService, kPath, kInterface,
                                                       QStringLiteral("Notify"));
    call << p->label << entry.id << appIcon << p->title << p->body.toHtmlEscaped() << p->actions
         << hints << (p->ongoing ? 0 : -1);
    const QDBusMessage reply = QDBusConnection::sessionBus().call(call, QDBus::Block, 5000);
    if (reply.type() != QDBusMessage::ReplyMessage || reply.arguments().isEmpty()) {
        qCWarning(lcNotify) << "Notify failed:" << reply.errorMessage();
        return;
    }
    const uint id = reply.arguments().constFirst().toUInt();
    if (entry.id && entry.id != id)
        m_keys.remove(entry.id);
    entry.id          = id;
    entry.digest      = digest.result();
    entry.replyAction = p->replyAction;
    entry.autoCancel  = p->autoCancel;
    m_entries.insert(p->key, entry);
    m_keys.insert(id, p->key);
    save();
}

void Notifications::remove(const QString &key) {
    const auto it = m_entries.constFind(key);
    if (it == m_entries.constEnd())
        return;
    const uint id = it->id;
    m_entries.erase(it);
    m_keys.remove(id);
    close(id);
    save();
}

void Notifications::sync(const QJsonObject &message) {
    QSet<QString> live;
    const QJsonArray keys = message.value(QStringLiteral("keys")).toArray();
    for (const QJsonValue &key : keys)
        live.insert(key.toString());
    const QStringList known = m_entries.keys();
    for (const QString &key : known) {
        if (!live.contains(key))
            remove(key);
    }
}

void Notifications::icon(const QJsonObject &message) {
    const QString pkg = message.value(QStringLiteral("pkg")).toString();
    if (!Validate::isPackageName(pkg))
        return;
    const QImage image =
        Images::decode(Validate::base64(message, QStringLiteral("png"), 256 * 1024), 128);
    if (image.isNull()) {
        qCWarning(lcNotify) << "rejected the icon of" << pkg;
        return;
    }
    Images::save(image, m_iconDir + QLatin1Char('/') + pkg + QStringLiteral(".png"));
}

void Notifications::close(uint id) {
    QDBusMessage call = QDBusMessage::createMethodCall(kService, kPath, kInterface,
                                                       QStringLiteral("CloseNotification"));
    call << id;
    QDBusConnection::sessionBus().asyncCall(call);
}

void Notifications::onActionInvoked(uint id, const QString &action) {
    if (action == QLatin1String("inline-reply"))
        return;
    QString key = m_keys.value(id);
    if (!key.isEmpty())
        m_entries[key].invokedAt = QDateTime::currentMSecsSinceEpoch();
    else if (const auto it = m_closing.find(id); it != m_closing.end())
        key = it->key, it->entry.invokedAt = QDateTime::currentMSecsSinceEpoch();
    if (key.isEmpty())
        return;
    qCInfo(lcNotify) << "invoke" << action << "on" << key;
    m_bridge->send({{QStringLiteral("t"), QStringLiteral("invoke")},
                    {QStringLiteral("key"), key},
                    {QStringLiteral("action"), action.left(16)}});
}

void Notifications::onNotificationReplied(uint id, const QString &text) {
    const QString key = m_keys.value(id);
    if (key.isEmpty() || m_entries.value(key).replyAction.isEmpty())
        return;
    m_bridge->send({{QStringLiteral("t"), QStringLiteral("reply")},
                    {QStringLiteral("key"), key},
                    {QStringLiteral("action"), m_entries.value(key).replyAction},
                    {QStringLiteral("text"), text.left(4096)}});
}

void Notifications::onNotificationClosed(uint id, uint reason) {
    const QString key = m_keys.take(id);
    if (key.isEmpty())
        return;
    const Entry entry = m_entries.take(key);
    save();
    // 2: the user dismissed it. The Hub also closes a row it opens, and may
    // say so before it reports the tap: wait for the tap before deciding.
    if (reason != 2)
        return;
    m_closing.insert(id, {key, entry});
    QTimer::singleShot(500, this, [this, id] {
        const Closing closing = m_closing.take(id);
        // Android keeps a tapped notification that doesn't auto-cancel.
        if (!closing.entry.autoCancel &&
            QDateTime::currentMSecsSinceEpoch() - closing.entry.invokedAt < 3000)
            return;
        qCInfo(lcNotify) << "dismiss" << closing.key;
        m_bridge->send(
            {{QStringLiteral("t"), QStringLiteral("dismiss")}, {QStringLiteral("key"), closing.key}});
    });
}

void Notifications::load() {
    QFile file(m_stateFile);
    if (!file.open(QIODevice::ReadOnly))
        return;
    const QJsonObject state = QJsonDocument::fromJson(file.readAll()).object();
    for (auto it = state.constBegin(); it != state.constEnd(); ++it) {
        const QJsonObject o = it.value().toObject();
        Entry             entry;
        entry.id          = uint(o.value(QStringLiteral("id")).toDouble());
        entry.digest      = QByteArray::fromHex(o.value(QStringLiteral("digest")).toString().toLatin1());
        entry.replyAction = o.value(QStringLiteral("reply")).toString();
        entry.autoCancel  = o.value(QStringLiteral("autoCancel")).toBool();
        if (!entry.id)
            continue;
        m_entries.insert(it.key(), entry);
        m_keys.insert(entry.id, it.key());
    }
    qCInfo(lcNotify) << "restored" << m_entries.size() << "notifications";
}

void Notifications::save() const {
    QJsonObject state;
    for (auto it = m_entries.constBegin(); it != m_entries.constEnd(); ++it) {
        state.insert(it.key(), QJsonObject{{QStringLiteral("id"), double(it->id)},
                                           {QStringLiteral("digest"), QString::fromLatin1(it->digest.toHex())},
                                           {QStringLiteral("reply"), it->replyAction},
                                           {QStringLiteral("autoCancel"), it->autoCancel}});
    }
    QDir().mkpath(QFileInfo(m_stateFile).absolutePath());
    QSaveFile file(m_stateFile);
    if (file.open(QIODevice::WriteOnly)) {
        file.write(QJsonDocument(state).toJson(QJsonDocument::Compact));
        file.commit();
    }
}
