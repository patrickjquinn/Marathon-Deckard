#include "permissionprompts.h"

#include "permissionmap.h"
#include "validate.h"

#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QJsonArray>
#include <QLoggingCategory>
#include <QRegularExpression>

Q_LOGGING_CATEGORY(lcPrompts, "deckard.permissions")

namespace {
    const QString kShell = QStringLiteral("org.marathonos.Shell");
    const QString kPath  = QStringLiteral("/org/marathonos/Shell/Permissions");
    const QString kIface = QStringLiteral("org.marathonos.Shell.Permissions1");

    bool isId(const QString &id) {
        static const QRegularExpression re(QStringLiteral("^[0-9a-f]{16}$"));
        return re.match(id).hasMatch();
    }
} // namespace

PermissionPrompts::PermissionPrompts(QDBusConnection bus, Send send, QObject *parent)
    : QObject(parent)
    , m_bus(bus)
    , m_send(std::move(send)) {}

bool PermissionPrompts::prompt(const QJsonObject &ask, QString *appId, QString *permission,
                               QVariantMap *options) {
    static const QStringList kKinds      = {QStringLiteral("foreground"), QStringLiteral("background"),
                                            QStringLiteral("precise")};
    static const QStringList kAccuracies = {QString(), QStringLiteral("choice"), QStringLiteral("approximate")};
    const QString pkg      = ask.value(QStringLiteral("pkg")).toString();
    const QString kind     = ask.value(QStringLiteral("kind")).toString();
    const QJsonValue accuracyValue = ask.value(QStringLiteral("accuracy"));
    const QString accuracy = accuracyValue.toString();
    if (!Validate::isPackageName(pkg) || !kKinds.contains(kind))
        return false;
    if ((!accuracyValue.isUndefined() && !accuracyValue.isString()) || !kAccuracies.contains(accuracy))
        return false;
    for (const char *flag : {"once", "preciseDefault"})
        if (!ask.value(QLatin1String(flag)).isUndefined() && !ask.value(QLatin1String(flag)).isBool())
            return false;

    const QJsonArray perms = ask.value(QStringLiteral("perms")).toArray();
    QString          name;
    if (perms.isEmpty() || perms.size() > 16)
        return false;
    for (const QJsonValue &p : perms) {
        const QString mapped = PermissionMap::marathon(p.toString());
        if (mapped.isEmpty() || (!name.isEmpty() && mapped != name))
            return false;
        name = mapped;
    }
    *appId      = QStringLiteral("waydroid.") + pkg;
    *permission = name;
    *options    = {{QStringLiteral("request"), ask.value(QStringLiteral("id")).toString()},
                   {QStringLiteral("kind"), kind},
                   {QStringLiteral("once"), ask.value(QStringLiteral("once")).toBool()},
                   {QStringLiteral("accuracy"), accuracy},
                   {QStringLiteral("preciseDefault"), ask.value(QStringLiteral("preciseDefault")).toBool()}};
    return true;
}

void PermissionPrompts::handle(const QString &type, const QJsonObject &message) {
    const QString id = message.value(QStringLiteral("id")).toString();
    if (!isId(id))
        return;
    if (type == QLatin1String("perm_cancel")) {
        if (m_waiting.remove(id))
            cancel(id);
        return;
    }
    if (type != QLatin1String("perm_ask") || m_waiting.contains(id))
        return;

    QString     appId;
    QString     permission;
    QVariantMap options;
    if (!prompt(message, &appId, &permission, &options) || m_waiting.size() >= kMaxWaiting) {
        qCInfo(lcPrompts) << "left to Android:" << message.value(QStringLiteral("pkg")).toString().left(255)
                          << message.value(QStringLiteral("perms")).toArray().size() << "permissions";
        answer(id, QStringLiteral("unsupported"));
        return;
    }
    const int generation = ++m_generation;
    m_waiting.insert(id, generation);
    QDBusMessage call = QDBusMessage::createMethodCall(kShell, kPath, kIface, QStringLiteral("PromptAndroid"));
    call << appId << permission << options;
    auto *watcher = new QDBusPendingCallWatcher(m_bus.asyncCall(call, kTimeoutMs), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, id, generation](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        if (m_waiting.value(id) != generation)
            return;
        m_waiting.remove(id);
        const QDBusPendingReply<QVariantMap> reply = *w;
        if (reply.isError()) {
            const bool timedOut = reply.error().type() == QDBusError::NoReply;
            qCWarning(lcPrompts) << "PromptAndroid:" << reply.error().message();
            answer(id, timedOut ? QStringLiteral("cancel") : QStringLiteral("unsupported"));
            return;
        }
        const QString a = reply.value().value(QStringLiteral("answer")).toString();
        static const QStringList kAnswers = {QStringLiteral("allow"), QStringLiteral("once"),
                                             QStringLiteral("deny"), QStringLiteral("cancel")};
        answer(id, kAnswers.contains(a) ? a : QStringLiteral("cancel"),
               reply.value().value(QStringLiteral("precise")).toBool());
    });
}

void PermissionPrompts::systemLost() {
    const QStringList ids = m_waiting.keys();
    m_waiting.clear();
    for (const QString &id : ids)
        cancel(id);
}

void PermissionPrompts::answer(const QString &id, const QString &answer, bool precise) {
    m_send({{QStringLiteral("t"), QStringLiteral("perm_answer")},
            {QStringLiteral("id"), id},
            {QStringLiteral("answer"), answer},
            {QStringLiteral("precise"), precise}});
}

void PermissionPrompts::cancel(const QString &id) {
    QDBusMessage call = QDBusMessage::createMethodCall(kShell, kPath, kIface, QStringLiteral("CancelAndroidPrompt"));
    call << id;
    m_bus.call(call, QDBus::NoBlock);
}
