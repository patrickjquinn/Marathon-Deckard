#include "manage.h"
#include "validate.h"

#include <QDateTime>
#include <QJsonArray>
#include <QLoggingCategory>
#include <QRegularExpression>
#include <QSet>
#include <QTimer>

Q_LOGGING_CATEGORY(lcManage, "deckard.manage")

namespace ManageCheck {

    const QStringList &groups() {
        static const QStringList kGroups = {
            QStringLiteral("location"), QStringLiteral("camera"),   QStringLiteral("microphone"),
            QStringLiteral("contacts"), QStringLiteral("calendar"), QStringLiteral("phone"),
            QStringLiteral("call_log"), QStringLiteral("sms"),      QStringLiteral("storage"),
            QStringLiteral("photos"),   QStringLiteral("music"),    QStringLiteral("nearby"),
            QStringLiteral("sensors"),  QStringLiteral("activity")};
        return kGroups;
    }

    const QStringList &specialKinds() {
        static const QStringList kKinds = {
            QStringLiteral("all_files"),     QStringLiteral("install_apps"),
            QStringLiteral("overlay"),       QStringLiteral("write_settings"),
            QStringLiteral("usage"),         QStringLiteral("notification_listener"),
            QStringLiteral("dnd"),           QStringLiteral("alarms"),
            QStringLiteral("pip"),           QStringLiteral("battery"),
            QStringLiteral("accessibility"), QStringLiteral("device_admin")};
        return kKinds;
    }

    bool isGroup(const QString &group) {
        return groups().contains(group);
    }

    bool isSpecialKind(const QString &kind) {
        return specialKinds().contains(kind);
    }

    bool isBatteryMode(const QString &mode) {
        return mode == QLatin1String("unrestricted") || mode == QLatin1String("optimized");
    }

    namespace {
        qint64 bytes(const QJsonObject &o, const QString &key) {
            return qBound<qint64>(0, qint64(o.value(key).toDouble()), qint64(1) << 50);
        }

        bool isDomain(const QString &d) {
            static const QRegularExpression kDomain(
                QStringLiteral("^(\\*\\.)?[A-Za-z0-9-]+(\\.[A-Za-z0-9-]+)+$"));
            return d.size() <= 253 && kDomain.match(d).hasMatch();
        }
    } // namespace

    QVariantMap app(const QJsonObject &r) {
        const QString pkg = r.value(QStringLiteral("pkg")).toString();
        if (!Validate::isPackageName(pkg))
            return {};
        QString label = Validate::label(r, QStringLiteral("label"));
        if (label.trimmed().isEmpty())
            label = pkg;

        QVariantList  permissions;
        QSet<QString> seen;
        const QJsonArray perms = r.value(QStringLiteral("permissions")).toArray();
        for (const QJsonValue &v : perms) {
            const QJsonObject p     = v.toObject();
            const QString     group = p.value(QStringLiteral("group")).toString();
            if (!isGroup(group) || seen.contains(group))
                continue;
            seen.insert(group);
            permissions.append(QVariantMap{
                {QStringLiteral("group"), group},
                {QStringLiteral("granted"), p.value(QStringLiteral("granted")).toBool()},
                {QStringLiteral("fixed"), p.value(QStringLiteral("fixed")).toBool()}});
        }

        QStringList      domains;
        const QJsonArray ds = r.value(QStringLiteral("domains")).toArray();
        for (const QJsonValue &v : ds) {
            const QString d = v.toString();
            if (isDomain(d) && !domains.contains(d) && domains.size() < 50)
                domains.append(d);
        }

        const QString battery = r.value(QStringLiteral("battery")).toString();
        return {
            {QStringLiteral("pkg"), pkg},
            {QStringLiteral("label"), label},
            {QStringLiteral("versionName"), Validate::text(r, QStringLiteral("versionName"), 64)},
            {QStringLiteral("versionCode"), qBound<qint64>(0, qint64(r.value(QStringLiteral("versionCode")).toDouble()), qint64(1) << 53)},
            {QStringLiteral("system"), r.value(QStringLiteral("system")).toBool()},
            {QStringLiteral("appBytes"), bytes(r, QStringLiteral("appBytes"))},
            {QStringLiteral("dataBytes"), bytes(r, QStringLiteral("dataBytes"))},
            {QStringLiteral("cacheBytes"), bytes(r, QStringLiteral("cacheBytes"))},
            {QStringLiteral("notifications"), r.value(QStringLiteral("notifications")).toBool()},
            {QStringLiteral("links"), r.value(QStringLiteral("links")).toBool()},
            {QStringLiteral("domains"), domains},
            {QStringLiteral("battery"), isBatteryMode(battery) ? battery : QStringLiteral("optimized")},
            {QStringLiteral("permissions"), permissions},
        };
    }

    QVariantList special(const QJsonObject &r) {
        QVariantList     out;
        QSet<QString>    seen;
        const QJsonArray apps = r.value(QStringLiteral("apps")).toArray();
        for (const QJsonValue &v : apps) {
            const QJsonObject a   = v.toObject();
            const QString     pkg = a.value(QStringLiteral("pkg")).toString();
            if (!Validate::isPackageName(pkg) || seen.contains(pkg) || out.size() >= 500)
                continue;
            seen.insert(pkg);
            QString label = Validate::label(a, QStringLiteral("label"));
            if (label.trimmed().isEmpty())
                label = pkg;
            out.append(QVariantMap{{QStringLiteral("pkg"), pkg},
                                   {QStringLiteral("label"), label},
                                   {QStringLiteral("granted"), a.value(QStringLiteral("granted")).toBool()}});
        }
        return out;
    }

    QVariantMap info(const QJsonObject &r) {
        return {{QStringLiteral("release"), Validate::text(r, QStringLiteral("release"), 16)},
                {QStringLiteral("sdk"), qBound(0, r.value(QStringLiteral("sdk")).toInt(), 99)}};
    }

} // namespace ManageCheck

Manage::Manage(Send send, QObject *parent)
    : QObject(parent)
    , m_send(std::move(send))
    , m_timer(new QTimer(this)) {
    m_timer->setInterval(1000);
    connect(m_timer, &QTimer::timeout, this, &Manage::expire);
}

bool Manage::isConnected() const {
    return m_connected;
}

void Manage::request(const QString &op, QJsonObject args, Done done) {
    if (!m_connected) {
        done(false, {{QStringLiteral("error"), QStringLiteral("Android isn't running")}});
        return;
    }
    if (m_pending.size() >= kMaxPending) {
        done(false, {{QStringLiteral("error"), QStringLiteral("busy")}});
        return;
    }
    const QString id = QStringLiteral("%1").arg(++m_next, 16, 16, QLatin1Char('0'));
    m_pending.insert(id, {std::move(done), QDateTime::currentMSecsSinceEpoch() + kTimeoutMs});
    if (!m_timer->isActive())
        m_timer->start();
    args.insert(QStringLiteral("t"), QStringLiteral("manage"));
    args.insert(QStringLiteral("id"), id);
    args.insert(QStringLiteral("op"), op);
    m_send(args);
}

void Manage::handle(const QString &type, const QJsonObject &message) {
    if (type != QLatin1String("managed"))
        return;
    const QString id = message.value(QStringLiteral("id")).toString();
    if (!m_pending.contains(id))
        return;
    QJsonObject reply = message;
    if (!message.value(QStringLiteral("ok")).toBool())
        reply.insert(QStringLiteral("error"), Validate::text(message, QStringLiteral("error"), 256));
    finish(id, message.value(QStringLiteral("ok")).toBool(), reply);
}

void Manage::setConnected(bool connected) {
    if (connected == m_connected)
        return;
    m_connected = connected;
    if (!connected) {
        const QStringList ids = m_pending.keys();
        for (const QString &id : ids)
            finish(id, false, {{QStringLiteral("error"), QStringLiteral("Android stopped")}});
    }
    Q_EMIT connectedChanged(connected);
}

void Manage::expire() {
    const qint64      now = QDateTime::currentMSecsSinceEpoch();
    const QStringList ids = m_pending.keys();
    for (const QString &id : ids) {
        if (m_pending.value(id).deadline <= now) {
            qCWarning(lcManage) << "request" << id << "timed out";
            finish(id, false, {{QStringLiteral("error"), QStringLiteral("Android didn't answer")}});
        }
    }
    if (m_pending.isEmpty())
        m_timer->stop();
}

void Manage::finish(const QString &id, bool ok, const QJsonObject &reply) {
    const Pending p = m_pending.take(id);
    if (p.done)
        p.done(ok, reply);
}
