#pragma once

#include <QDBusAbstractAdaptor>
#include <QDBusConnection>
#include <QJsonObject>
#include <QStringList>
#include <QVariantMap>

#include <functional>

class Callers;
class Manage;
class Runtime;
class QDBusMessage;

// org.marathonos.Deckard1 on /org/marathonos/Deckard: what Marathon's
// Settings shows and changes about Android apps (protocol/bridge.md,
// Settings API). GetRuntime and AppIcon answer anyone on the session bus;
// everything else only Settings.
class SettingsApi : public QDBusAbstractAdaptor {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.marathonos.Deckard1")

  public:
    static const QStringList &privileged();

    SettingsApi(QObject *root, QDBusConnection bus, Callers *callers, Manage *manage,
                Runtime *runtime, QStringList iconDirs);

  public Q_SLOTS:
    QVariantMap GetRuntime();
    void        Start(const QDBusMessage &message);
    void        Stop(const QDBusMessage &message);
    void        Restart(const QDBusMessage &message);
    void        SetStartAtLogin(bool enabled, const QDBusMessage &message);

    QString     AppIcon(const QString &pkg);
    QVariantMap GetApp(const QString &pkg, const QDBusMessage &message);
    void SetPermission(const QString &pkg, const QString &group, bool granted, const QDBusMessage &message);
    void SetNotifications(const QString &pkg, bool enabled, const QDBusMessage &message);
    void SetOpenLinks(const QString &pkg, bool enabled, const QDBusMessage &message);
    void SetBattery(const QString &pkg, const QString &mode, const QDBusMessage &message);
    void ForceStop(const QString &pkg, const QDBusMessage &message);
    void ClearCache(const QString &pkg, const QDBusMessage &message);
    void ClearData(const QString &pkg, const QDBusMessage &message);

    QList<QVariantMap> GetSpecialAccess(const QString &kind, const QDBusMessage &message);
    void SetSpecialAccess(const QString &kind, const QString &pkg, bool granted, const QDBusMessage &message);

    void ResetAndroid(const QDBusMessage &message);

  Q_SIGNALS:
    void RuntimeChanged(const QVariantMap &runtime);
    void AppChanged(const QString &pkg);

  private:
    using Answer = std::function<QVariant(const QJsonObject &reply)>;

    bool invalid(const QDBusMessage &message, bool bad);
    void privilegedRun(const QDBusMessage &message, std::function<void()> run);
    void forward(const QDBusMessage &message, const QString &op, const QJsonObject &args,
                 const QString &changedPkg, Answer answer = nullptr);

    QDBusConnection m_bus;
    Callers        *m_callers;
    Manage         *m_manage;
    Runtime        *m_runtime;
    QStringList     m_iconDirs;
};
