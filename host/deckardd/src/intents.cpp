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
            const QString text = Validate::text(QJsonObject{{QStringLiteral("q"), query}}, QStringLiteral("q"), 256).trimmed();
            if (!text.isEmpty())
                return Target{Target::Keel, QStringLiteral("maps"), QStringLiteral("search.show"),
                              {{QStringLiteral("query"), text}}};
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
        // Special access: Marathon Settings' page for the kind, for one app when it asked.
        static const QHash<QString, QString> access{
            {QStringLiteral("android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION"), QStringLiteral("all_files")},
            {QStringLiteral("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION"), QStringLiteral("all_files")},
            {QStringLiteral("android.settings.MANAGE_UNKNOWN_APP_SOURCES"), QStringLiteral("install_apps")},
            {QStringLiteral("android.settings.action.MANAGE_OVERLAY_PERMISSION"), QStringLiteral("overlay")},
            {QStringLiteral("android.settings.action.MANAGE_WRITE_SETTINGS"), QStringLiteral("write_settings")},
            {QStringLiteral("android.settings.USAGE_ACCESS_SETTINGS"), QStringLiteral("usage")},
            {QStringLiteral("android.settings.ACTION_NOTIFICATION_LISTENER_SETTINGS"), QStringLiteral("notification_listener")},
            {QStringLiteral("android.settings.NOTIFICATION_LISTENER_DETAIL_SETTINGS"), QStringLiteral("notification_listener")},
            {QStringLiteral("android.settings.NOTIFICATION_POLICY_ACCESS_SETTINGS"), QStringLiteral("dnd")},
            {QStringLiteral("android.settings.IGNORE_BATTERY_OPTIMIZATION_SETTINGS"), QStringLiteral("battery")},
            {QStringLiteral("android.settings.REQUEST_IGNORE_BATTERY_OPTIMIZATIONS"), QStringLiteral("battery")},
            {QStringLiteral("android.settings.REQUEST_SCHEDULE_EXACT_ALARM"), QStringLiteral("alarms")},
            {QStringLiteral("android.settings.PICTURE_IN_PICTURE_SETTINGS"), QStringLiteral("pip")},
            {QStringLiteral("android.settings.ACCESSIBILITY_SETTINGS"), QStringLiteral("accessibility")},
        };
        if (const QString kind = access.value(a); !kind.isEmpty())
            return QStringLiteral("androidaccess/") + kind +
                   (Validate::isPackageName(pkg) ? QStringLiteral("/") + pkg : QString());
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
            QString       body;
            if (!isTel) {
                body = str(m, QStringLiteral("body"), 10000);
                if (body.isEmpty()) {
                    QJsonObject fromUri{{QStringLiteral("body"),
                                         QUrlQuery(uri.section(u'?', 1)).queryItemValue(QStringLiteral("body"),
                                                                                        QUrl::FullyDecoded)}};
                    body = str(fromUri, QStringLiteral("body"), 10000);
                }
            }
            if (n.isEmpty() && body.isEmpty())
                return Target{Target::Launch, app, {}, {}};
            QJsonObject args;
            if (!n.isEmpty())
                args.insert(QStringLiteral("number"), n);
            if (!body.isEmpty())
                args.insert(QStringLiteral("body"), body);
            if (isTel)
                return Target{Target::Route, app, QStringLiteral("/call"), args};
            return Target{Target::Route, app,
                          n.isEmpty() ? QStringLiteral("compose") : QStringLiteral("conversation"), args};
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
    QDir().mkpath(m_xfer);
    sweep();
    m_bus.connect(QStringLiteral("org.marathonos.Shell"), QStringLiteral("/org/marathonos/Shell/Navigation"),
                  QStringLiteral("org.marathonos.Shell.Navigation1"), QStringLiteral("ForegroundAppChanged"),
                  this, SLOT(onForegroundApp(QString)));
}

// Android's focus normally follows the hwcomposer's keyboard enter; this
// tells DeckardSystem which Android app Marathon has in front, so it can
// correct Android when that enter never came.
void SystemIntents::onForegroundApp(const QString &appId) {
    const QString prefix = QStringLiteral("waydroid.");
    const QString pkg    = appId.startsWith(prefix) ? appId.mid(prefix.size()) : QString();
    if (pkg == m_focusPkg)
        return;
    m_focusPkg = Validate::isPackageName(pkg) ? pkg : QString();
    resendFocus();
}

void SystemIntents::resendFocus() {
    if (!m_focusPkg.isEmpty())
        m_send({{QStringLiteral("t"), QStringLiteral("focus")}, {QStringLiteral("pkg"), m_focusPkg}});
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
        m_pending.insert(id, p);
        capture(id, p.kind);
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

// Marathon Camera's camera.capture answers once the person has shot, with
// the file's path; only files in its folder, of the kind asked for, go to
// Android.
void SystemIntents::capture(const QString &id, const QString &kind) {
    QDBusMessage call = QDBusMessage::createMethodCall(
        QStringLiteral("org.marathonos.camera"), QStringLiteral("/org/shipwright/Keel/Actions"),
        QStringLiteral("org.shipwright.Keel.Actions"), QStringLiteral("Invoke"));
    call << QStringLiteral("camera.capture")
         << QString::fromUtf8(QJsonDocument(QJsonObject{{QStringLiteral("mode"),
                                                          kind == QLatin1String("video") ? QStringLiteral("video")
                                                                                         : QStringLiteral("photo")}})
                                  .toJson(QJsonDocument::Compact))
         << QVariantMap();
    auto *watcher = new QDBusPendingCallWatcher(m_bus.asyncCall(call, int(kCaptureTimeout + 30000)), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, id, kind, watcher] {
        watcher->deleteLater();
        if (!m_pending.contains(id) || m_pending.value(id).mode != QLatin1String("capture"))
            return;
        const QDBusMessage reply = watcher->reply();
        if (reply.type() != QDBusMessage::ReplyMessage || reply.arguments().isEmpty()) {
            qCInfo(lcIntents) << "capture" << id << "ended:" << reply.errorName();
            finish(id, {}, false);
            return;
        }
        const QString path = QJsonDocument::fromJson(reply.arguments().constFirst().toString().toUtf8())
                                 .object()
                                 .value(QStringLiteral("path"))
                                 .toString();
        static const QStringList images{QStringLiteral("jpg"), QStringLiteral("jpeg"), QStringLiteral("png")};
        static const QStringList videos{QStringLiteral("mp4"), QStringLiteral("mkv"), QStringLiteral("webm"),
                                        QStringLiteral("mov")};
        const QFileInfo file(path);
        const QString   folder = QFileInfo(m_cameraDir).canonicalFilePath();
        const bool      inside = !folder.isEmpty() && file.canonicalPath() == folder && file.isFile();
        const bool      right  = (kind == QLatin1String("video") ? videos : images).contains(file.suffix().toLower());
        if (!inside || !right) {
            qCWarning(lcIntents) << "capture" << id << "refused" << path;
            finish(id, {}, false);
            return;
        }
        finish(id, {file.canonicalFilePath()}, true);
    });
    qCInfo(lcIntents) << "capture" << kind << id;
}

void SystemIntents::drop(const QString &id) {
    const auto it = m_pending.constFind(id);
    if (it == m_pending.cend())
        return;
    if (!it->portalPath.isEmpty()) {
        m_bus.disconnect(QString(), it->portalPath, QStringLiteral("org.freedesktop.portal.Request"),
                         QStringLiteral("Response"), this, SLOT(onPortalResponse(QDBusMessage)));
        // The app gave up: take the chooser off the screen too.
        m_bus.asyncCall(QDBusMessage::createMethodCall(QStringLiteral("org.freedesktop.portal.Desktop"),
                                                       it->portalPath,
                                                       QStringLiteral("org.freedesktop.portal.Request"),
                                                       QStringLiteral("Close")));
    }
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
