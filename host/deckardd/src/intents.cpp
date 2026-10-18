#include "intents.h"

#include "validate.h"

#include <QDBusArgument>
#include <QDBusMessage>
#include <QDBusMetaType>
#include <QDBusPendingCallWatcher>
#include <QDateTime>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonArray>
#include <QJsonDocument>
#include <QLoggingCategory>
#include <QRegularExpression>
#include <QTimer>
#include <QUrl>
#include <QUrlQuery>

Q_LOGGING_CATEGORY(lcIntents, "deckard.intents")

namespace {

    constexpr int    kMaxPending     = 4;
    constexpr qint64 kCaptureTimeout = 10 * 60 * 1000;
    constexpr qint64 kXferMaxAgeDays = 30;

    struct PortalRule {
        uint    type = 1; // 0 glob, 1 MIME type
        QString pattern;
    };
    struct PortalFilter {
        QString           name;
        QList<PortalRule> rules;
    };

    QDBusArgument &operator<<(QDBusArgument &a, const PortalRule &r) {
        a.beginStructure();
        a << r.type << r.pattern;
        a.endStructure();
        return a;
    }
    const QDBusArgument &operator>>(const QDBusArgument &a, PortalRule &r) {
        a.beginStructure();
        a >> r.type >> r.pattern;
        a.endStructure();
        return a;
    }
    QDBusArgument &operator<<(QDBusArgument &a, const PortalFilter &f) {
        a.beginStructure();
        a << f.name << f.rules;
        a.endStructure();
        return a;
    }
    const QDBusArgument &operator>>(const QDBusArgument &a, PortalFilter &f) {
        a.beginStructure();
        a >> f.name >> f.rules;
        a.endStructure();
        return a;
    }

} // namespace

Q_DECLARE_METATYPE(PortalRule)

Q_DECLARE_METATYPE(PortalFilter)

namespace Intents {

    namespace {

        QString str(const QJsonObject &o, const QString &key, int max) {
            return Validate::text(o, key, max);
        }

        QStringList addresses(const QJsonObject &o, const QString &key) {
            static const QRegularExpression address(
                QStringLiteral("^[^\\s,<>\"]{1,254}$"));
            QStringList out;
            const QJsonArray list = o.value(key).toArray();
            for (const QJsonValue &v : list) {
                const QString a = v.toString().trimmed();
                if (address.match(a).hasMatch() && out.size() < 20)
                    out << a;
            }
            return out;
        }

        // The number in a tel:, sms: or smsto: URI, or "" if it has none.
        QString number(const QString &uri) {
            static const QRegularExpression allowed(QStringLiteral("[^0-9+*#,;pw]"));
            QString rest = QUrl::fromPercentEncoding(uri.section(u':', 1).toUtf8());
            rest         = rest.section(u'?', 0, 0).section(u',', 0, 0).remove(QLatin1Char(' '));
            rest.remove(QRegularExpression(QStringLiteral("[().-]")));
            if (rest.contains(allowed) || rest.size() > 64)
                return {};
            return rest;
        }

        // geo:lat,lon[;u=..][?q=lat,lon(label)|text]
        std::optional<Target> geo(const QString &uri) {
            const QString body  = uri.mid(4);
            const QString where = body.section(u'?', 0, 0).section(u';', 0, 0);
            const QString query = QUrlQuery(body.section(u'?', 1)).queryItemValue(
                QStringLiteral("q"), QUrl::FullyDecoded);
            static const QRegularExpression point(
                QStringLiteral("^\\s*(-?\\d{1,3}(?:\\.\\d+)?)\\s*,\\s*(-?\\d{1,3}(?:\\.\\d+)?)\\s*(?:\\((.*)\\))?\\s*$"));
            auto show = [](const QRegularExpressionMatch &m) -> std::optional<Target> {
                const double lat = m.captured(1).toDouble();
                const double lon = m.captured(2).toDouble();
                if (lat < -90 || lat > 90 || lon < -180 || lon > 180 || (lat == 0 && lon == 0))
                    return std::nullopt;
                QJsonObject args{{QStringLiteral("lat"), lat}, {QStringLiteral("lon"), lon}};
                const QString name = m.captured(3).left(256);
                if (!name.isEmpty())
                    args.insert(QStringLiteral("name"), name);
                return Target{Target::Keel, QStringLiteral("maps"), QStringLiteral("map.show"), args};
            };
            if (auto t = show(point.match(query)))
                return t;
            if (auto t = show(point.match(where)))
                return t;
            return Target{Target::Launch, QStringLiteral("maps"), {}, {}};
        }

    } // namespace

    QString settingsRoute(const QString &a, const QString &pkg) {
        static const QHash<QString, QString> pages{
            {QStringLiteral("android.settings.WIFI_SETTINGS"), QStringLiteral("wifi")},
            {QStringLiteral("android.settings.WIRELESS_SETTINGS"), QStringLiteral("wifi")},
            {QStringLiteral("android.settings.panel.action.INTERNET_CONNECTIVITY"), QStringLiteral("wifi")},
            {QStringLiteral("android.settings.panel.action.WIFI"), QStringLiteral("wifi")},
            {QStringLiteral("android.settings.BLUETOOTH_SETTINGS"), QStringLiteral("bluetooth")},
            {QStringLiteral("android.settings.SOUND_SETTINGS"), QStringLiteral("sound")},
            {QStringLiteral("android.settings.panel.action.VOLUME"), QStringLiteral("sound")},
            {QStringLiteral("android.settings.DISPLAY_SETTINGS"), QStringLiteral("display")},
            {QStringLiteral("android.settings.DATE_SETTINGS"), QStringLiteral("datetime")},
            {QStringLiteral("android.settings.INTERNAL_STORAGE_SETTINGS"), QStringLiteral("storage")},
            {QStringLiteral("android.settings.MEMORY_CARD_SETTINGS"), QStringLiteral("storage")},
            {QStringLiteral("android.settings.BATTERY_SAVER_SETTINGS"), QStringLiteral("battery")},
            {QStringLiteral("android.intent.action.POWER_USAGE_SUMMARY"), QStringLiteral("battery")},
            {QStringLiteral("android.settings.SECURITY_SETTINGS"), QStringLiteral("security")},
            {QStringLiteral("android.settings.INPUT_METHOD_SETTINGS"), QStringLiteral("keyboard")},
            {QStringLiteral("android.settings.SYNC_SETTINGS"), QStringLiteral("accounts")},
            {QStringLiteral("android.settings.ADD_ACCOUNT_SETTINGS"), QStringLiteral("accounts")},
            {QStringLiteral("android.settings.DEVICE_INFO_SETTINGS"), QStringLiteral("about")},
            {QStringLiteral("android.settings.NOTIFICATION_SETTINGS"), QStringLiteral("notifications")},
            {QStringLiteral("android.settings.ZEN_MODE_SETTINGS"), QStringLiteral("notifications")},
            {QStringLiteral("android.settings.APP_NOTIFICATION_SETTINGS"), QStringLiteral("notifications")},
            {QStringLiteral("android.settings.CHANNEL_NOTIFICATION_SETTINGS"), QStringLiteral("notifications")},
            {QStringLiteral("android.settings.APPLICATION_DETAILS_SETTINGS"), QStringLiteral("appmanager")},
            {QStringLiteral("android.settings.MANAGE_APPLICATIONS_SETTINGS"), QStringLiteral("appmanager")},
            {QStringLiteral("android.settings.MANAGE_ALL_APPLICATIONS_SETTINGS"), QStringLiteral("appmanager")},
        };
        const QString page = pages.value(a);
        if (page.isEmpty())
            return {};
        const bool perApp = page == QLatin1String("appmanager") || page == QLatin1String("notifications");
        if (perApp && Validate::isPackageName(pkg))
            return page + QStringLiteral("/waydroid.") + pkg;
        return page;
    }

    bool isRequestId(const QString &id) {
        static const QRegularExpression re(QStringLiteral("^[0-9a-f]{16}$"));
        return re.match(id).hasMatch();
    }

    QString safeName(const QString &name) {
        QString n = QFileInfo(name).fileName();
        n.remove(QRegularExpression(QStringLiteral("[\\x00-\\x1f/\\\\]")));
        n = n.left(200).trimmed();
        if (n.isEmpty() || n == QLatin1String(".") || n == QLatin1String(".."))
            return QStringLiteral("file");
        return n;
    }

    std::optional<Target> target(const QJsonObject &m, QString *error) {
        const QString kind = m.value(QStringLiteral("kind")).toString();
        const QString uri  = m.value(QStringLiteral("uri")).toString();
        if (uri.size() > 4096) {
            *error = QStringLiteral("uri too long");
            return std::nullopt;
        }
        const QString scheme = uri.section(u':', 0, 0).toLower();
        if (kind == QLatin1String("url")) {
            const QUrl url(uri, QUrl::StrictMode);
            if (!url.isValid() || (scheme != QLatin1String("http") && scheme != QLatin1String("https")) ||
                url.host().isEmpty() || uri.size() > 2048) {
                *error = QStringLiteral("not a web address");
                return std::nullopt;
            }
            return Target{Target::Keel, QStringLiteral("browser"), QStringLiteral("browser.open"),
                          {{QStringLiteral("url"), url.toString(QUrl::FullyEncoded)},
                           {QStringLiteral("newTab"), true}}};
        }
        if (kind == QLatin1String("tel") || kind == QLatin1String("sms")) {
            static const QStringList tel{QStringLiteral("tel")};
            static const QStringList sms{QStringLiteral("sms"), QStringLiteral("smsto"),
                                         QStringLiteral("mms"), QStringLiteral("mmsto")};
            const bool isTel = kind == QLatin1String("tel");
            if (!(isTel ? tel : sms).contains(scheme)) {
                *error = QStringLiteral("wrong scheme for ") + kind;
                return std::nullopt;
            }
            const QString app = isTel ? QStringLiteral("phone") : QStringLiteral("messages");
            const QString n   = number(uri);
            if (n.isEmpty())
                return Target{Target::Launch, app, {}, {}};
            return Target{Target::Route, app,
                          isTel ? QStringLiteral("/call") : QStringLiteral("conversation"),
                          {{QStringLiteral("number"), n}}};
        }
        if (kind == QLatin1String("mailto")) {
            QJsonObject args;
            const QString to = addresses(m, QStringLiteral("to")).join(u',');
            const QString cc = addresses(m, QStringLiteral("cc")).join(u',');
            if (!to.isEmpty())
                args.insert(QStringLiteral("to"), to);
            if (!cc.isEmpty())
                args.insert(QStringLiteral("cc"), cc);
            const QString subject = str(m, QStringLiteral("subject"), 1000);
            const QString body    = str(m, QStringLiteral("body"), 100000);
            if (!subject.isEmpty())
                args.insert(QStringLiteral("subject"), subject);
            if (!body.isEmpty())
                args.insert(QStringLiteral("body"), body);
            return Target{Target::Keel, QStringLiteral("email"), QStringLiteral("mail.compose"), args};
        }
        if (kind == QLatin1String("geo")) {
            if (scheme != QLatin1String("geo")) {
                *error = QStringLiteral("not a geo: URI");
                return std::nullopt;
            }
            return geo(uri);
        }
        if (kind == QLatin1String("settings")) {
            const QString route = settingsRoute(m.value(QStringLiteral("action")).toString(),
                                                m.value(QStringLiteral("pkg")).toString());
            if (route.isEmpty())
                return Target{Target::Launch, QStringLiteral("settings"), {}, {}};
            return Target{Target::Route, QStringLiteral("settings"), route, {}};
        }
        *error = QStringLiteral("unknown kind ") + kind.left(32);
        return std::nullopt;
    }

} // namespace Intents

void SystemIntents::callLogged(const QDBusMessage &call) {
    auto *watcher = new QDBusPendingCallWatcher(m_bus.asyncCall(call, 30000), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this,
            [what = call.service() + u' ' + call.member()](QDBusPendingCallWatcher *w) {
                if (w->isError())
                    qCWarning(lcIntents) << what << "failed:" << w->error().message();
                w->deleteLater();
            });
}

SystemIntents::SystemIntents(Send send, const QString &xferDir, const QString &cameraDir,
                             QDBusConnection bus, QObject *parent)
    : QObject(parent)
    , m_send(std::move(send))
    , m_xfer(xferDir)
    , m_cameraDir(cameraDir)
    , m_bus(bus) {
    qDBusRegisterMetaType<PortalRule>();
    qDBusRegisterMetaType<QList<PortalRule>>();
    qDBusRegisterMetaType<PortalFilter>();
    qDBusRegisterMetaType<QList<PortalFilter>>();
    m_cameraPoll = new QTimer(this);
    m_cameraPoll->setInterval(1000);
    connect(m_cameraPoll, &QTimer::timeout, this, &SystemIntents::pollCamera);
    QDir().mkpath(m_xfer);
    sweep();
}

void SystemIntents::handle(const QString &type, const QJsonObject &message) {
    if (type == QLatin1String("open"))
        open(message);
    else if (type == QLatin1String("pick"))
        pick(message);
    else if (type == QLatin1String("pick_done"))
        pickDone(message.value(QStringLiteral("id")).toString());
    else if (type == QLatin1String("pick_cancel"))
        drop(message.value(QStringLiteral("id")).toString());
}

void SystemIntents::cancelAll() {
    const QStringList ids = m_pending.keys();
    for (const QString &id : ids)
        drop(id);
}

void SystemIntents::open(const QJsonObject &message) {
    QString    error;
    const auto t = Intents::target(message, &error);
    if (!t) {
        qCWarning(lcIntents) << "open refused:" << error;
        return;
    }
    QDBusMessage call;
    if (t->kind == Intents::Target::Keel) {
        call = QDBusMessage::createMethodCall(QStringLiteral("org.marathonos.") + t->app,
                                              QStringLiteral("/org/shipwright/Keel/Actions"),
                                              QStringLiteral("org.shipwright.Keel.Actions"),
                                              QStringLiteral("Invoke"));
        call << t->name << QString::fromUtf8(QJsonDocument(t->args).toJson(QJsonDocument::Compact))
             << QVariantMap();
    } else {
        const bool route = t->kind == Intents::Target::Route;
        call = QDBusMessage::createMethodCall(
            QStringLiteral("org.marathonos.Shell"), QStringLiteral("/org/marathonos/Shell/Navigation"),
            QStringLiteral("org.marathonos.Shell.Navigation1"),
            route ? QStringLiteral("LaunchAppWithRoute") : QStringLiteral("LaunchApp"));
        call << t->app;
        if (route)
            call << t->name << QString::fromUtf8(QJsonDocument(t->args).toJson(QJsonDocument::Compact));
    }
    qCInfo(lcIntents) << "open" << t->app << t->name;
    callLogged(call);
}

void SystemIntents::pick(const QJsonObject &m) {
    const QString id   = m.value(QStringLiteral("id")).toString();
    const QString mode = m.value(QStringLiteral("mode")).toString();
    if (!Intents::isRequestId(id) || m_pending.contains(id) || m_pending.size() >= kMaxPending) {
        qCWarning(lcIntents) << "pick refused";
        if (Intents::isRequestId(id) && !m_pending.contains(id))
            finish(id, {}, false);
        return;
    }
    Pending p;
    p.mode = mode;
    if (mode == QLatin1String("capture")) {
        p.kind = m.value(QStringLiteral("kind")).toString() == QLatin1String("video")
                     ? QStringLiteral("video")
                     : QStringLiteral("image");
        QDir().mkpath(m_cameraDir);
        const QStringList before = QDir(m_cameraDir).entryList(QDir::Files);
        p.cameraBefore           = QSet<QString>(before.cbegin(), before.cend());
        p.deadline               = QDateTime::currentMSecsSinceEpoch() + kCaptureTimeout;
        m_pending.insert(id, p);
        QDBusMessage call = QDBusMessage::createMethodCall(
            QStringLiteral("org.marathonos.camera"), QStringLiteral("/org/shipwright/Keel/Actions"),
            QStringLiteral("org.shipwright.Keel.Actions"), QStringLiteral("Invoke"));
        call << QStringLiteral("camera.open")
             << QString::fromUtf8(QJsonDocument(QJsonObject{
                                      {QStringLiteral("mode"),
                                       p.kind == QLatin1String("video") ? QStringLiteral("video")
                                                                        : QStringLiteral("photo")}})
                                      .toJson(QJsonDocument::Compact))
             << QVariantMap();
        callLogged(call);
        m_cameraPoll->start();
        return;
    }
    if (mode != QLatin1String("open") && mode != QLatin1String("save")) {
        finish(id, {}, false);
        return;
    }

    const QString token = QStringLiteral("deckard_") + id;
    QString       sender = m_bus.baseService().mid(1);
    sender.replace(u'.', u'_');
    p.portalPath = QStringLiteral("/org/freedesktop/portal/desktop/request/%1/%2").arg(sender, token);
    m_bus.connect(QString(), p.portalPath, QStringLiteral("org.freedesktop.portal.Request"),
                  QStringLiteral("Response"), this, SLOT(onPortalResponse(QDBusMessage)));

    QVariantMap options{{QStringLiteral("handle_token"), token}};
    QString     method;
    if (mode == QLatin1String("open")) {
        method = QStringLiteral("OpenFile");
        options.insert(QStringLiteral("multiple"), m.value(QStringLiteral("multiple")).toBool());
        static const QRegularExpression mime(QStringLiteral("^[a-z0-9.+-]{1,64}/[a-z0-9.+*-]{1,64}$"));
        PortalFilter filter{QStringLiteral("Files"), {}};
        const QJsonArray types = m.value(QStringLiteral("mime")).toArray();
        bool             any   = types.isEmpty();
        for (const QJsonValue &v : types) {
            const QString t = v.toString().toLower();
            if (t == QLatin1String("*/*"))
                any = true;
            else if (mime.match(t).hasMatch() && filter.rules.size() < 16)
                filter.rules << PortalRule{1, t};
        }
        if (!any && !filter.rules.isEmpty())
            options.insert(QStringLiteral("filters"),
                           QVariant::fromValue(QList<PortalFilter>{filter}));
    } else {
        method = QStringLiteral("SaveFile");
        options.insert(QStringLiteral("current_name"),
                       Intents::safeName(Validate::text(m, QStringLiteral("name"), 255)));
    }
    m_pending.insert(id, p);
    QDBusMessage call = QDBusMessage::createMethodCall(
        QStringLiteral("org.freedesktop.portal.Desktop"), QStringLiteral("/org/freedesktop/portal/desktop"),
        QStringLiteral("org.freedesktop.portal.FileChooser"), method);
    call << QString() << (mode == QLatin1String("open") ? tr("Open") : tr("Save")) << options;
    callLogged(call);
    qCInfo(lcIntents) << "pick" << mode << id;
}

void SystemIntents::onPortalResponse(const QDBusMessage &message) {
    QString id;
    for (auto it = m_pending.cbegin(); it != m_pending.cend(); ++it)
        if (it->portalPath == message.path())
            id = it.key();
    if (id.isEmpty() || message.arguments().size() < 2)
        return;
    const uint        response = message.arguments().at(0).toUInt();
    const QVariantMap results  = qdbus_cast<QVariantMap>(message.arguments().at(1));
    QStringList       files;
    for (const QString &uri : results.value(QStringLiteral("uris")).toStringList()) {
        const QUrl url(uri);
        if (url.isLocalFile())
            files << url.toLocalFile();
    }
    if (m_pending.value(id).mode == QLatin1String("save")) {
        if (response == 0 && !files.isEmpty()) {
            m_saved.insert(id, files.first());
            m_send({{QStringLiteral("t"), QStringLiteral("picked")},
                    {QStringLiteral("id"), id},
                    {QStringLiteral("ok"), true},
                    {QStringLiteral("files"),
                     QJsonArray{QJsonObject{{QStringLiteral("name"),
                                             Intents::safeName(files.first())}}}}});
            m_bus.disconnect(QString(), message.path(), QStringLiteral("org.freedesktop.portal.Request"),
                             QStringLiteral("Response"), this, SLOT(onPortalResponse(QDBusMessage)));
            m_pending.remove(id);
            return;
        }
        finish(id, {}, false);
        return;
    }
    finish(id, files, response == 0 && !files.isEmpty());
}

// Copies the chosen host files into xfer/<id>/ for DeckardSystem's provider
// and answers the request.
void SystemIntents::finish(const QString &id, const QStringList &hostFiles, bool ok) {
    QJsonArray out;
    if (ok) {
        const QString dir = m_xfer + u'/' + id;
        QDir().mkpath(dir);
        for (const QString &path : hostFiles) {
            QString name = Intents::safeName(path);
            for (int n = 1; QFile::exists(dir + u'/' + name); n++)
                name = QString::number(n) + u'-' + Intents::safeName(path);
            if (!QFile::copy(path, dir + u'/' + name)) {
                qCWarning(lcIntents) << "copy failed" << path;
                continue;
            }
            out.append(QJsonObject{{QStringLiteral("name"), name},
                                   {QStringLiteral("size"), QFileInfo(dir + u'/' + name).size()}});
        }
    }
    m_send({{QStringLiteral("t"), QStringLiteral("picked")},
            {QStringLiteral("id"), id},
            {QStringLiteral("ok"), ok && !out.isEmpty()},
            {QStringLiteral("files"), out}});
    drop(id);
}

void SystemIntents::pickDone(const QString &id) {
    const QString target = m_saved.value(id);
    if (!Intents::isRequestId(id) || target.isEmpty())
        return;
    const QString name = Intents::safeName(target);
    const QString from = m_xfer + u'/' + id + u'/' + name;
    QFile::remove(target + QStringLiteral(".deckard-part"));
    if (!QFile::copy(from, target + QStringLiteral(".deckard-part")) ||
        (QFile::exists(target) && !QFile::remove(target)) ||
        !QFile::rename(target + QStringLiteral(".deckard-part"), target))
        qCWarning(lcIntents) << "save failed" << target;
    else
        qCInfo(lcIntents) << "saved" << target;
}

// A capture is answered with the first new photo or video in Marathon
// Camera's folder once its size stops changing.
void SystemIntents::pollCamera() {
    const qint64      now = QDateTime::currentMSecsSinceEpoch();
    const QStringList ids = m_pending.keys();
    bool              waiting = false;
    for (const QString &id : ids) {
        Pending &p = m_pending[id];
        if (p.mode != QLatin1String("capture"))
            continue;
        if (now > p.deadline) {
            finish(id, {}, false);
            continue;
        }
        waiting = true;
        static const QStringList images{QStringLiteral("jpg"), QStringLiteral("jpeg"), QStringLiteral("png")};
        static const QStringList videos{QStringLiteral("mp4"), QStringLiteral("mkv"), QStringLiteral("webm"),
                                        QStringLiteral("mov")};
        const QStringList &wanted = p.kind == QLatin1String("video") ? videos : images;
        const QFileInfoList entries = QDir(m_cameraDir).entryInfoList(QDir::Files, QDir::Time);
        for (const QFileInfo &f : entries) {
            if (p.cameraBefore.contains(f.fileName()) || !wanted.contains(f.suffix().toLower()))
                continue;
            const qint64 size = f.size();
            if (size > 0 && p.growing.value(f.fileName(), -1) == size) {
                finish(id, {f.absoluteFilePath()}, true);
                waiting = false;
                break;
            }
            p.growing.insert(f.fileName(), size);
            break;
        }
    }
    if (!waiting)
        m_cameraPoll->stop();
}

void SystemIntents::drop(const QString &id) {
    const auto it = m_pending.constFind(id);
    if (it == m_pending.cend())
        return;
    if (!it->portalPath.isEmpty())
        m_bus.disconnect(QString(), it->portalPath, QStringLiteral("org.freedesktop.portal.Request"),
                         QStringLiteral("Response"), this, SLOT(onPortalResponse(QDBusMessage)));
    m_pending.erase(it);
}

// Transfers older than a month are gone; an app that kept a grant to one
// gets "file not found".
void SystemIntents::sweep() {
    const QDateTime   cutoff = QDateTime::currentDateTime().addDays(-kXferMaxAgeDays);
    const QFileInfoList dirs = QDir(m_xfer).entryInfoList(QDir::Dirs | QDir::NoDotAndDotDot);
    for (const QFileInfo &d : dirs)
        if (Intents::isRequestId(d.fileName()) && d.lastModified() < cutoff)
            QDir(d.absoluteFilePath()).removeRecursively();
}
