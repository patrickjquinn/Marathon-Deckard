#pragma once

#include <QDBusAbstractAdaptor>
#include <QDBusMessage>
#include <QHash>
#include <QJsonObject>
#include <QObject>
#include <QVariantMap>

#include <functional>

using PackageList = QList<QVariantMap>;

// Android apps installed the Marathon way: the Store or Settings asks over
// D-Bus, DeckardBridge does the work through PackageInstaller sessions with no
// Android UI. org.marathonos.Deckard1.Packages on /org/marathonos/Deckard
// (protocol/bridge.md).
class Packages : public QDBusAbstractAdaptor {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.marathonos.Deckard1.Packages")

  public:
    using Send      = std::function<bool(const QJsonObject &)>;
    // Runs run for a caller allowed to install and remove apps; otherwise it
    // answers the call itself (AccessDenied).
    using MayChange = std::function<void(const QDBusMessage &, std::function<void()> run)>;

    static constexpr qint64 kMaxApk = Q_INT64_C(1) << 30;

    // root: the object exported at /org/marathonos/Deckard.
    Packages(QObject *root, Send send, MayChange mayChange, const QString &installDir);

    void handle(const QString &type, const QJsonObject &message);
    void bridgeLost();

    // Whether process pid runs the executable at path (same file).
    static bool runs(qint64 pid, const QString &path);

  public Q_SLOTS:
    // {ok, message, pkg, label, versionName, versionCode, minSdk, targetSdk,
    //  permissions (as), icon (PNG bytes), installedVersionCode,
    //  installedVersionName, and what it needs: features, libraries, abis
    //  (as), gms (b)}
    QVariantMap InspectPackage(const QString &path, const QDBusMessage &call);
    // A job id; Progress and Finished follow.
    QString InstallPackage(const QString &path, const QDBusMessage &call);
    QString UninstallPackage(const QString &pkg, const QDBusMessage &call);
    // This phone's Android: {features, libraries, abis (as), sdk, gms}.
    QVariantMap DeviceProfile(const QDBusMessage &call);
    // {pkg, label, versionName, versionCode, system, launchable} for every package.
    PackageList ListPackages(const QDBusMessage &call);

  Q_SIGNALS:
    void Progress(const QString &job, int percent);
    void Finished(const QString &job, bool ok, const QString &pkg, const QString &message);
    // change: installed, updated or removed.
    void PackageChanged(const QString &pkg, const QString &change);

  protected:
    virtual void sendReply(const QDBusMessage &reply);

  private:
    struct Pending {
        QDBusMessage call; // for replies; empty for jobs
        QString      type;
        QString      staged;
    };

    QString stage(const QString &path, QString *error);
    QString start(const QJsonObject &message, const Pending &pending, QString *error);
    void    startJob(const QDBusMessage &call, const QJsonObject &message, const Pending &pending);
    void    reply(const QString &id, const QVariant &value);
    void    done(const QString &id, bool ok, const QString &pkg, const QString &message);

    Send                    m_send;
    MayChange               m_mayChange;
    QString                 m_dir;
    QHash<QString, Pending> m_pending;
};
