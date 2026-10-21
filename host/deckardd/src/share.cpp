#include "share.h"

#include "images.h"
#include "intents.h"
#include "validate.h"

#include <QBuffer>
#include <QDBusArgument>
#include <QDBusMetaType>
#include <QDBusPendingCallWatcher>
#include <QDBusUnixFileDescriptor>
#include <QDBusVariant>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QLoggingCategory>
#include <QMimeDatabase>
#include <QRandomGenerator>
#include <QRegularExpression>
#include <QTimer>

#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

Q_LOGGING_CATEGORY(lcShare, "deckard.share")

namespace {
    const QString kFailed = QStringLiteral("org.marathonos.Deckard1.Error.Failed");

    QString newId() {
        return QStringLiteral("%1").arg(QRandomGenerator::global()->generate64(), 16, 16, QLatin1Char('0'));
    }

    bool isMimeType(const QString &mime) {
        static const QRegularExpression re(QStringLiteral("^[\\w.+-]+/[\\w.+*-]+$"));
        return mime.size() <= 128 && re.match(mime).hasMatch();
    }

    bool isClassName(const QString &name) {
        static const QRegularExpression re(
            QStringLiteral("^\\.?[A-Za-z_$][\\w$]*(\\.[A-Za-z_$][\\w$]*)*$"));
        return re.match(name).hasMatch();
    }

    QString iconUri(const QByteArray &png) {
        const QImage image = Images::decode(png, 96);
        if (image.isNull())
            return {};
        QByteArray out;
        QBuffer    buffer(&out);
        buffer.open(QIODevice::WriteOnly);
        image.save(&buffer, "PNG");
        return QStringLiteral("data:image/png;base64,") + QString::fromLatin1(out.toBase64());
    }

    // An fd for a regular file directly in dir, never through a link.
    int openIn(const QString &dir, const QString &name) {
        const int dfd = ::open(QFile::encodeName(dir).constData(), O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
        if (dfd < 0)
            return -1;
        const int fd = ::openat(dfd, QFile::encodeName(name).constData(), O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
        ::close(dfd);
        struct stat st {};
        if (fd >= 0 && (::fstat(fd, &st) != 0 || !S_ISREG(st.st_mode))) {
            ::close(fd);
            return -1;
        }
        return fd;
    }
} // namespace

Sharing::Sharing(QObject *root, QDBusConnection bus, Send send, IsShell isShell, const QString &xferDir)
    : QDBusAbstractAdaptor(root)
    , m_bus(std::move(bus))
    , m_send(std::move(send))
    , m_isShell(std::move(isShell))
    , m_xfer(xferDir) {
    qDBusRegisterMetaType<ShareTargetList>();
}

bool Sharing::isTargetId(const QString &id) {
    const int slash = id.indexOf(u'/');
    return id.size() <= 256 && slash > 0 && Validate::isPackageName(id.left(slash)) &&
           isClassName(id.mid(slash + 1));
}

QVariant Sharing::plain(const QVariant &value) {
    if (value.metaType() == QMetaType::fromType<QDBusVariant>())
        return plain(value.value<QDBusVariant>().variant());
    if (value.metaType() == QMetaType::fromType<QDBusArgument>()) {
        const QDBusArgument arg = value.value<QDBusArgument>();
        if (arg.currentType() == QDBusArgument::MapType) {
            QVariantMap map;
            arg.beginMap();
            while (!arg.atEnd()) {
                arg.beginMapEntry();
                const QVariant key  = arg.asVariant();
                const QVariant item = arg.asVariant();
                arg.endMapEntry();
                map.insert(key.toString(), plain(item));
            }
            arg.endMap();
            return map;
        }
        if (arg.currentType() == QDBusArgument::ArrayType) {
            QVariantList list;
            arg.beginArray();
            while (!arg.atEnd())
                list.append(plain(arg.asVariant()));
            arg.endArray();
            return list;
        }
        return value;
    }
    if (value.metaType() == QMetaType::fromType<QVariantMap>()) {
        QVariantMap map = value.toMap();
        for (auto it = map.begin(); it != map.end(); ++it)
            it.value() = plain(it.value());
        return map;
    }
    if (value.metaType() == QMetaType::fromType<QVariantList>()) {
        QVariantList list = value.toList();
        for (QVariant &item : list)
            item = plain(item);
        return list;
    }
    return value;
}

void Sharing::sendReply(const QDBusMessage &reply) {
    m_bus.send(reply);
}

bool Sharing::refuse(const QDBusMessage &call) {
    if (m_isShell(call))
        return false;
    sendReply(call.createErrorReply(QDBusError::AccessDenied, QStringLiteral("only the shell shares")));
    return true;
}

void Sharing::start(const QString &id, const QJsonObject &message, const Pending &pending) {
    m_pending.insert(id, pending);
    if (!m_send(message)) {
        fail(id, QStringLiteral("Android isn't running"));
        return;
    }
    QTimer::singleShot(kTimeoutMs, this, [this, id] { fail(id, QStringLiteral("Android didn't answer")); });
}

void Sharing::fail(const QString &id, const QString &message) {
    const auto it = m_pending.constFind(id);
    if (it == m_pending.constEnd())
        return;
    const Pending p = m_pending.take(id);
    if (p.type == QLatin1String("targets")) {
        sendReply(p.call.createReply(QVariant::fromValue(ShareTargetList())));
        return;
    }
    QDir(m_xfer + u'/' + id).removeRecursively();
    qCWarning(lcShare) << "share failed:" << message;
    sendReply(p.call.createErrorReply(kFailed, message));
}

ShareTargetList Sharing::ShareTargets(const QString &mimeType, const QDBusMessage &call) {
    call.setDelayedReply(true);
    if (refuse(call))
        return {};
    if (!isMimeType(mimeType)) {
        sendReply(call.createErrorReply(QDBusError::InvalidArgs, QStringLiteral("not a MIME type")));
        return {};
    }
    const QString id = newId();
    start(id, {{QStringLiteral("t"), QStringLiteral("share_targets")}, {QStringLiteral("id"), id}, {QStringLiteral("mime"), mimeType}},
          {call, QStringLiteral("targets")});
    return {};
}

void Sharing::targetsResult(const QString &id, const QJsonObject &message) {
    const Pending   p = m_pending.take(id);
    ShareTargetList list;
    const QJsonArray targets = message.value(QStringLiteral("targets")).toArray();
    for (const QJsonValue &v : targets) {
        const QJsonObject o        = v.toObject();
        const QString     pkg      = o.value(QStringLiteral("pkg")).toString();
        const QString     activity = o.value(QStringLiteral("activity")).toString();
        const QString     target   = pkg + u'/' + activity;
        if (!isTargetId(target) || pkg.startsWith(QLatin1String("org.marathonos.deckard.")))
            continue;
        list.append({{QStringLiteral("id"), target},
                     {QStringLiteral("pkg"), pkg},
                     {QStringLiteral("label"), Validate::label(o, QStringLiteral("label"))},
                     {QStringLiteral("icon"), iconUri(Validate::base64(o, QStringLiteral("icon"), 256 * 1024))},
                     {QStringLiteral("multiple"), o.value(QStringLiteral("multiple")).toBool()}});
        if (list.size() >= kMaxTargets)
            break;
    }
    sendReply(p.call.createReply(QVariant::fromValue(list)));
}

QString Sharing::copyIn(const QString &tid, const QVariantList &resources, QStringList *files, QString *text,
                        qint64 *bytes) {
    const QString dir = m_xfer + u'/' + tid;
    QStringList   texts;
    for (const QVariant &resource : resources) {
        const QVariantMap map = resource.toMap();
        if (!map.contains(QStringLiteral("fileDescriptor"))) {
            const QString data = map.value(QStringLiteral("data")).toString();
            if (data.isEmpty() || data.size() > kMaxText)
                return QStringLiteral("text of 1 to 65536 characters");
            texts << data;
            continue;
        }
        const QDBusUnixFileDescriptor fd = map.value(QStringLiteral("fileDescriptor")).value<QDBusUnixFileDescriptor>();
        struct stat st {};
        if (!fd.isValid() || ::fstat(fd.fileDescriptor(), &st) != 0 || !S_ISREG(st.st_mode))
            return QStringLiteral("not a file");
        *bytes += st.st_size;
        if (*bytes > kMaxBytes)
            return QStringLiteral("too large");
        QString name = Intents::safeName(map.value(QStringLiteral("name")).toString());
        for (int n = 2; files->contains(name); ++n)
            name = QStringLiteral("%1-%2").arg(n).arg(Intents::safeName(map.value(QStringLiteral("name")).toString()));
        if (!QDir().mkpath(dir))
            return QStringLiteral("cannot write the transfer");
        QFile out(dir + u'/' + name);
        if (!out.open(QIODevice::WriteOnly | QIODevice::Truncate))
            return QStringLiteral("cannot write the transfer");
        QByteArray buffer(256 * 1024, Qt::Uninitialized);
        off_t      offset = 0;
        for (;;) {
            const ssize_t n = ::pread(fd.fileDescriptor(), buffer.data(), buffer.size(), offset);
            if (n < 0)
                return QStringLiteral("cannot read a file");
            if (n == 0)
                break;
            if (out.write(buffer.constData(), n) != n)
                return QStringLiteral("cannot write the transfer");
            offset += n;
            if (offset > st.st_size)
                return QStringLiteral("a file grew while it was copied");
        }
        files->append(name);
    }
    *text = texts.join(u'\n');
    return {};
}

void Sharing::Share(const QString &id, const QVariantMap &configuration, const QDBusMessage &call) {
    call.setDelayedReply(true);
    if (refuse(call))
        return;
    const QVariantMap  config    = plain(configuration).toMap();
    const QVariant     raw       = config.value(QStringLiteral("resources"));
    const QVariantList resources = raw.metaType() == QMetaType::fromType<QVariantList>() ? raw.toList() : QVariantList();
    QString            mime      = config.value(QStringLiteral("mimeType")).toString();
    if (!isTargetId(id) || resources.isEmpty() || resources.size() > kMaxResources ||
        (!mime.isEmpty() && !isMimeType(mime))) {
        sendReply(call.createErrorReply(QDBusError::InvalidArgs, QStringLiteral("a target and 1 to 32 resources")));
        return;
    }
    const QString tid = newId();
    QStringList   files;
    QString       text;
    qint64        bytes = 0;
    const QString error = copyIn(tid, resources, &files, &text, &bytes);
    if (!error.isEmpty()) {
        QDir(m_xfer + u'/' + tid).removeRecursively();
        sendReply(call.createErrorReply(QDBusError::InvalidArgs, error));
        return;
    }
    if (mime.isEmpty())
        mime = files.isEmpty() ? QStringLiteral("text/plain") : QMimeDatabase().mimeTypeForFile(files.first(), QMimeDatabase::MatchExtension).name();
    const QString     pkg = id.section(u'/', 0, 0);
    const QJsonObject message{{QStringLiteral("t"), QStringLiteral("share")},
                              {QStringLiteral("id"), tid},
                              {QStringLiteral("target"), id},
                              {QStringLiteral("mime"), mime},
                              {QStringLiteral("title"), config.value(QStringLiteral("title")).toString().left(256)},
                              {QStringLiteral("text"), text},
                              {QStringLiteral("files"), QJsonArray::fromStringList(files)}};
    qCInfo(lcShare) << "sharing" << resources.size() << "items with" << id;

    // The shell lets the window Android opens for the share take the front.
    QDBusMessage expect = QDBusMessage::createMethodCall(
        QStringLiteral("org.marathonos.Shell"), QStringLiteral("/org/marathonos/Shell/Navigation"),
        QStringLiteral("org.marathonos.Shell.Navigation1"), QStringLiteral("ExpectAppWindow"));
    expect << QStringLiteral("waydroid.") + pkg;
    auto *watcher = new QDBusPendingCallWatcher(m_bus.asyncCall(expect, 2000), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, tid, message, call](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        start(tid, message, {call, QStringLiteral("share")});
    });
}

void Sharing::shareOut(const QJsonObject &m) {
    const QString id  = m.value(QStringLiteral("id")).toString();
    const QString pkg = m.value(QStringLiteral("pkg")).toString();
    if (!Intents::isRequestId(id) || !Validate::isPackageName(pkg))
        return;
    const QString dir = m_xfer + u'/' + id;
    QVariantList  resources;
    const QJsonArray files = m.value(QStringLiteral("files")).toArray();
    for (const QJsonValue &v : files) {
        const QString name = v.toString();
        if (name.isEmpty() || name != Intents::safeName(name) || resources.size() >= kMaxResources)
            continue;
        const int fd = openIn(dir, name);
        if (fd < 0)
            continue;
        resources << QVariantMap{{QStringLiteral("name"), name},
                                 {QStringLiteral("fileDescriptor"), QVariant::fromValue(QDBusUnixFileDescriptor(fd))}};
        ::close(fd);
    }
    const QString text  = Validate::text(m, QStringLiteral("text"), kMaxText);
    const QString title = Validate::text(m, QStringLiteral("title"), 256);
    if (!text.isEmpty() && resources.size() < kMaxResources)
        resources << QVariantMap{{QStringLiteral("name"), title}, {QStringLiteral("data"), text}};
    if (resources.isEmpty())
        return;
    QString mime = m.value(QStringLiteral("mime")).toString();
    if (!isMimeType(mime))
        mime.clear();
    QDBusMessage call = QDBusMessage::createMethodCall(QStringLiteral("org.sailfishos.share"), QStringLiteral("/"),
                                                       QStringLiteral("org.sailfishos.share"), QStringLiteral("share"));
    call << QVariantMap{{QStringLiteral("resources"), resources},
                        {QStringLiteral("mimeType"), mime},
                        {QStringLiteral("title"), title},
                        {QStringLiteral("source"), QStringLiteral("waydroid.") + pkg}};
    qCInfo(lcShare) << pkg << "shares" << resources.size() << "items";
    auto *watcher = new QDBusPendingCallWatcher(m_bus.asyncCall(call, 10000), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [pkg](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        if (w->isError())
            qCWarning(lcShare) << "the share sheet refused" << pkg << w->error().message();
    });
}

void Sharing::handle(const QString &type, const QJsonObject &message) {
    if (type == QLatin1String("share_out")) {
        shareOut(message);
        return;
    }
    const QString id = message.value(QStringLiteral("id")).toString();
    const auto    it = m_pending.constFind(id);
    if (it == m_pending.constEnd())
        return;
    if (type == QLatin1String("share_targets_result") && it->type == QLatin1String("targets")) {
        targetsResult(id, message);
    } else if (type == QLatin1String("shared") && it->type == QLatin1String("share")) {
        if (!message.value(QStringLiteral("ok")).toBool()) {
            fail(id, Validate::text(message, QStringLiteral("error"), 512));
            return;
        }
        sendReply(m_pending.take(id).call.createReply());
    }
}

void Sharing::systemLost() {
    const QStringList ids = m_pending.keys();
    for (const QString &id : ids)
        fail(id, QStringLiteral("Android stopped"));
}
