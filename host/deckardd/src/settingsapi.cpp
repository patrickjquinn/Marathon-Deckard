#include "settingsapi.h"
#include "callers.h"
#include "images.h"
#include "manage.h"
#include "runtime.h"
#include "validate.h"

#include <QBuffer>
#include <QDBusMessage>
#include <QDBusMetaType>
#include <QFile>

namespace {
    const QString kFailed = QStringLiteral("org.marathonos.Deckard1.Error.Failed");
} // namespace

const QStringList &SettingsApi::privileged() {
    static const QStringList kCallers = {QStringLiteral("settings")};
    return kCallers;
}

SettingsApi::SettingsApi(QObject *root, QDBusConnection bus, Callers *callers, Manage *manage,
                         Runtime *runtime, QStringList iconDirs)
    : QDBusAbstractAdaptor(root)
    , m_bus(bus)
    , m_callers(callers)
    , m_manage(manage)
    , m_runtime(runtime)
    , m_iconDirs(std::move(iconDirs)) {
    qDBusRegisterMetaType<QList<QVariantMap>>();
    setAutoRelaySignals(false);
    connect(runtime, &Runtime::changed, this, [this] { Q_EMIT RuntimeChanged(m_runtime->snapshot()); });
}

bool SettingsApi::invalid(const QDBusMessage &message, bool bad) {
    if (!bad)
        return false;
    message.setDelayedReply(true);
    m_bus.send(message.createErrorReply(QDBusError::InvalidArgs, QStringLiteral("Invalid arguments")));
    return true;
}

void SettingsApi::privilegedRun(const QDBusMessage &message, std::function<void()> run) {
    m_callers->require(message, privileged(), [this, message, run = std::move(run)] {
        run();
        m_bus.send(message.createReply());
    });
}

void SettingsApi::forward(const QDBusMessage &message, const QString &op, const QJsonObject &args,
                          const QString &changedPkg, Answer answer) {
    m_callers->require(message, privileged(), [=] {
        m_manage->request(op, args, [=](bool ok, const QJsonObject &reply) {
            if (!ok) {
                m_bus.send(message.createErrorReply(kFailed, reply.value(QStringLiteral("error")).toString()));
                return;
            }
            m_bus.send(answer ? message.createReply(answer(reply)) : message.createReply());
            if (!changedPkg.isEmpty())
                Q_EMIT AppChanged(changedPkg);
        });
    });
}

QVariantMap SettingsApi::GetRuntime() {
    return m_runtime->snapshot();
}

void SettingsApi::Start(const QDBusMessage &message) {
    privilegedRun(message, [this] { m_runtime->start(); });
}

void SettingsApi::Stop(const QDBusMessage &message) {
    privilegedRun(message, [this] { m_runtime->stop(); });
}

void SettingsApi::Restart(const QDBusMessage &message) {
    privilegedRun(message, [this] { m_runtime->restart(); });
}

void SettingsApi::SetStartAtLogin(bool enabled, const QDBusMessage &message) {
    privilegedRun(message, [this, enabled] { m_runtime->setStartAtLogin(enabled); });
}

QString SettingsApi::AppIcon(const QString &pkg) {
    if (!Validate::isPackageName(pkg))
        return {};
    for (const QString &dir : std::as_const(m_iconDirs)) {
        QFile f(dir + QLatin1Char('/') + pkg + QStringLiteral(".png"));
        if (f.size() > 512 * 1024 || !f.open(QIODevice::ReadOnly))
            continue;
        const QImage image = Images::decode(f.readAll(), 192);
        if (image.isNull())
            continue;
        QByteArray png;
        QBuffer    buffer(&png);
        buffer.open(QIODevice::WriteOnly);
        image.save(&buffer, "PNG");
        return QStringLiteral("data:image/png;base64,") + QString::fromLatin1(png.toBase64());
    }
    return {};
}

QVariantMap SettingsApi::GetApp(const QString &pkg, const QDBusMessage &message) {
    if (!invalid(message, !Validate::isPackageName(pkg)))
        forward(message, QStringLiteral("app"), {{QStringLiteral("pkg"), pkg}}, QString(),
                [](const QJsonObject &reply) { return QVariant(ManageCheck::app(reply)); });
    return {};
}

void SettingsApi::SetPermission(const QString &pkg, const QString &group, bool granted,
                                const QDBusMessage &message) {
    if (!invalid(message, !Validate::isPackageName(pkg) || !ManageCheck::isGroup(group)))
        forward(message, QStringLiteral("perm"),
                {{QStringLiteral("pkg"), pkg}, {QStringLiteral("group"), group}, {QStringLiteral("granted"), granted}},
                pkg);
}

void SettingsApi::SetNotifications(const QString &pkg, bool enabled, const QDBusMessage &message) {
    if (!invalid(message, !Validate::isPackageName(pkg)))
        forward(message, QStringLiteral("notifications"),
                {{QStringLiteral("pkg"), pkg}, {QStringLiteral("enabled"), enabled}}, pkg);
}

void SettingsApi::SetOpenLinks(const QString &pkg, bool enabled, const QDBusMessage &message) {
    if (!invalid(message, !Validate::isPackageName(pkg)))
        forward(message, QStringLiteral("links"),
                {{QStringLiteral("pkg"), pkg}, {QStringLiteral("enabled"), enabled}}, pkg);
}

void SettingsApi::SetBattery(const QString &pkg, const QString &mode, const QDBusMessage &message) {
    if (!invalid(message, !Validate::isPackageName(pkg) || !ManageCheck::isBatteryMode(mode)))
        forward(message, QStringLiteral("battery"),
                {{QStringLiteral("pkg"), pkg}, {QStringLiteral("mode"), mode}}, pkg);
}

void SettingsApi::ForceStop(const QString &pkg, const QDBusMessage &message) {
    if (!invalid(message, !Validate::isPackageName(pkg)))
        forward(message, QStringLiteral("stop"), {{QStringLiteral("pkg"), pkg}}, pkg);
}

void SettingsApi::ClearCache(const QString &pkg, const QDBusMessage &message) {
    if (!invalid(message, !Validate::isPackageName(pkg)))
        forward(message, QStringLiteral("cache"), {{QStringLiteral("pkg"), pkg}}, pkg);
}

void SettingsApi::ClearData(const QString &pkg, const QDBusMessage &message) {
    if (!invalid(message, !Validate::isPackageName(pkg)))
        forward(message, QStringLiteral("clear"), {{QStringLiteral("pkg"), pkg}}, pkg);
}

QList<QVariantMap> SettingsApi::GetSpecialAccess(const QString &kind, const QDBusMessage &message) {
    if (!invalid(message, !ManageCheck::isSpecialKind(kind)))
        forward(message, QStringLiteral("special"), {{QStringLiteral("kind"), kind}}, QString(),
                [](const QJsonObject &reply) {
                    QList<QVariantMap> apps;
                    const QVariantList clean = ManageCheck::special(reply);
                    for (const QVariant &a : clean)
                        apps.append(a.toMap());
                    return QVariant::fromValue(apps);
                });
    return {};
}

void SettingsApi::SetSpecialAccess(const QString &kind, const QString &pkg, bool granted,
                                   const QDBusMessage &message) {
    if (!invalid(message, !ManageCheck::isSpecialKind(kind) || !Validate::isPackageName(pkg)))
        forward(message, QStringLiteral("special_set"),
                {{QStringLiteral("kind"), kind}, {QStringLiteral("pkg"), pkg}, {QStringLiteral("granted"), granted}},
                pkg);
}

void SettingsApi::ResetAndroid(const QDBusMessage &message) {
    m_callers->require(message, privileged(), [this, message] {
        m_runtime->reset([this, message](bool ok, const QString &error) {
            m_bus.send(ok ? message.createReply() : message.createErrorReply(kFailed, error));
        });
    });
}
