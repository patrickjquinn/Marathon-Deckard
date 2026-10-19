#include "audio.h"
#include "bootwatchdog.h"
#include "bridge.h"
#include "callers.h"
#include "doze.h"
#include "ime.h"
#include "intents.h"
#include "keelapps.h"
#include "manage.h"
#include "mpris.h"
#include "notifications.h"
#include "packages.h"
#include "palette.h"
#include "reclaimer.h"
#include "runtime.h"
#include "sessionfollower.h"
#include "settingsapi.h"

#include <QCoreApplication>
#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QDBusError>
#include <QDBusMessage>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonObject>
#include <QLoggingCategory>
#include <QStandardPaths>
#include <QTimer>

// deckardd: Marathon's end of the Deckard system bridge (protocol/bridge.md).
int main(int argc, char *argv[]) {
    QCoreApplication app(argc, argv);
    app.setApplicationName(QStringLiteral("deckardd"));
    app.setApplicationVersion(QStringLiteral(PROJECT_VERSION));

    const QString data = qEnvironmentVariable(
        "DECKARD_WAYDROID_DATA",
        QStandardPaths::writableLocation(QStandardPaths::GenericDataLocation) +
            QStringLiteral("/waydroid/data"));
    if (!QDir(data).exists()) {
        qCritical("No Waydroid data directory at %s; has \"waydroid init\" run?", qPrintable(data));
        return 1;
    }
    const QString cache =
        QStandardPaths::writableLocation(QStandardPaths::GenericCacheLocation) +
        QStringLiteral("/deckard");
    const QString state =
        qEnvironmentVariable("XDG_STATE_HOME", QDir::homePath() + QStringLiteral("/.local/state")) +
        QStringLiteral("/deckard");

    QDBusConnection bus = QDBusConnection::sessionBus();
    if (!bus.registerService(QStringLiteral("org.marathonos.Deckard"))) {
        qCritical("org.marathonos.Deckard is taken: %s", qPrintable(bus.lastError().message()));
        return 1;
    }

    Bridge        bridge(data + QStringLiteral("/deckard/bridge.sock"),
                         data + QStringLiteral("/system/packages.list"));
    // DeckardSystem runs as Android's system uid, which is also this user's.
    Bridge        systemBridge(data + QStringLiteral("/deckard/system.sock"),
                               data + QStringLiteral("/system/packages.list"),
                               QByteArrayLiteral("org.marathonos.deckard.system"));
    Notifications notifications(&bridge, cache + QStringLiteral("/icons"),
                                state + QStringLiteral("/notifications.json"));
    Mpris         mpris(&bridge, cache + QStringLiteral("/art"));
    AudioSync     audio(&bridge);
    AmbiencePalette palette(data + QStringLiteral("/deckard/palette"));
    DozeSync      doze(data + QStringLiteral("/deckard/doze"));
    SessionFollower follower([] {
        QDBusMessage call = QDBusMessage::createMethodCall(
            QStringLiteral("org.freedesktop.systemd1"), QStringLiteral("/org/freedesktop/systemd1"),
            QStringLiteral("org.freedesktop.systemd1.Manager"), QStringLiteral("TryRestartUnit"));
        call << QStringLiteral("marathon-waydroid.service") << QStringLiteral("replace");
        QDBusConnection::sessionBus().send(call);
    });

    // /org/marathonos/Deckard: each feature adds its own interface.
    QObject  dbusRoot;
    Callers     callers(bus);
    // Installs and removals: Settings, the Store, and the Store's handler,
    // known by its executable since it runs outside any app. Removals go to
    // DeckardSystem: only the system uid may remove apps it did not install.
    Packages packages(
        &dbusRoot,
        [&bridge, &systemBridge](const QJsonObject &message) {
            Bridge &to = message.value(QStringLiteral("t")) == QLatin1String("uninstall") ? systemBridge : bridge;
            if (!to.isConnected())
                return false;
            to.send(message);
            return true;
        },
        [&bus, &callers](const QDBusMessage &call, std::function<void()> run) {
            const auto pid = bus.interface()->servicePid(call.service());
            if (pid.isValid() && Packages::runs(pid.value(), QStringLiteral("/usr/bin/marathon-store-handler"))) {
                run();
                return;
            }
            callers.require(call, {QStringLiteral("settings"), QStringLiteral("store")}, std::move(run));
        },
        data + QStringLiteral("/deckard/install"));
    if (!bus.registerObject(QStringLiteral("/org/marathonos/Deckard"), &dbusRoot,
                            QDBusConnection::ExportAdaptors))
        qWarning("cannot export /org/marathonos/Deckard");

    // Each Android app as a Keel Actions provider for keel-mcp and Pilot.
    KeelApps keelApps([&bridge](const QJsonObject &message) { bridge.send(message); },
                      [&systemBridge](const QJsonObject &message) { systemBridge.send(message); },
                      [](const QString &name) {
                          return QDBusConnection::connectToBus(QDBusConnection::SessionBus, name);
                      },
                      bus, {QStringLiteral("/usr/bin/keel-mcp"), QStringLiteral("/usr/bin/pilotd")},
                      state + QStringLiteral("/apps.json"));

    QObject::connect(&bridge, &Bridge::received, &app,
                     [&](const QString &type, const QJsonObject &message) {
                         if (type.startsWith(QLatin1String("media")))
                             mpris.handle(type, message);
                         else if (KeelApps::handles(type))
                             keelApps.handle(type, message);
                         else if (type.endsWith(QLatin1String("_result")) ||
                                  type == QLatin1String("install_progress") ||
                                  type == QLatin1String("package_changed"))
                             packages.handle(type, message);
                         else
                             notifications.handle(type, message);
                     });
    QObject::connect(&bridge, &Bridge::connectedChanged, &app, [&](bool connected) {
        keelApps.setConnected(connected);
        if (!connected) {
            mpris.clear();
            packages.bridgeLost();
            return;
        }
        bridge.send({{QStringLiteral("t"), QStringLiteral("hello")}, {QStringLiteral("v"), 1}});
        audio.resend();
    });
    if (!bridge.listen())
        return 1;

    // The keyboard has a socket of its own so keys never wait behind images.
    Bridge         imeBridge(data + QStringLiteral("/deckard/ime.sock"),
                             data + QStringLiteral("/system/packages.list"),
                             QByteArrayLiteral("org.marathonos.deckard.ime"));
    Ime            ime([&imeBridge](const QJsonObject &message) { imeBridge.send(message); });
    ShellTextInput shellTextInput(&ime, bus);
    QObject::connect(&imeBridge, &Bridge::received, &ime, &Ime::handle);
    QObject::connect(&imeBridge, &Bridge::connectedChanged, &ime, &Ime::setConnected);
    if (!imeBridge.listen())
        return 1;

    SystemIntents intents([&systemBridge](const QJsonObject &message) { systemBridge.send(message); },
                          data + QStringLiteral("/deckard/xfer"),
                          QStandardPaths::writableLocation(QStandardPaths::PicturesLocation) +
                              QStringLiteral("/Marathon"),
                          bus);
    QObject::connect(&systemBridge, &Bridge::received, &intents, &SystemIntents::handle);
    QObject::connect(&systemBridge, &Bridge::connectedChanged, &intents, [&](bool connected) {
        if (connected)
            intents.resendFocus();
        else
            intents.cancelAll();
    });
    QObject::connect(&systemBridge, &Bridge::received, &keelApps, [&](const QString &type, const QJsonObject &message) {
        if (KeelApps::handlesSystem(type))
            keelApps.handleSystem(type, message);
    });
    QObject::connect(&systemBridge, &Bridge::connectedChanged, &keelApps, &KeelApps::setSystemConnected);
    QObject::connect(&systemBridge, &Bridge::received, &packages, [&](const QString &type, const QJsonObject &message) {
        if (type == QLatin1String("install_result"))
            packages.handle(type, message);
    });
    if (!systemBridge.listen())
        return 1;

    const QString sessionUnit = QStringLiteral("marathon-waydroid.service");
    Manage        manage([&systemBridge](const QJsonObject &message) { systemBridge.send(message); });
    const QString cgroup = qEnvironmentVariable("DECKARD_CGROUP", QStringLiteral("/sys/fs/cgroup/lxc.payload.waydroid"));
    Runtime       runtime({cgroup + QStringLiteral("/memory.current"),
                           state + QStringLiteral("/settings.json"),
                           QStandardPaths::writableLocation(QStandardPaths::ApplicationsLocation)},
                          [&](const QString &method) { UnitWatch::call(bus, method, sessionUnit); });
    UnitWatch     unitWatch(bus, sessionUnit, &runtime);
    BootWatchdog  bootWatchdog({data + QStringLiteral("/deckard/booted"),
                                QFileInfo::exists(QStringLiteral("/usr/libexec/deckard-boot-evidence"))
                                    ? QStringList{QStringLiteral("pkexec"), QStringLiteral("/usr/libexec/deckard-boot-evidence")}
                                    : QStringList()},
                               [&runtime] { runtime.restart(); });
    QObject::connect(&runtime, &Runtime::changed, &bootWatchdog,
                     [&] { bootWatchdog.setActiveSince(runtime.activeSinceMs()); });
    Reclaimer     reclaimer({cgroup}, [&systemBridge](const QJsonObject &message) { systemBridge.send(message); });
    QObject::connect(&systemBridge, &Bridge::connectedChanged, &reclaimer, &Reclaimer::setConnected);
    QObject::connect(&systemBridge, &Bridge::received, &reclaimer, &Reclaimer::handle);
    QObject::connect(&systemBridge, &Bridge::received, &manage, &Manage::handle);
    QObject::connect(&systemBridge, &Bridge::connectedChanged, &manage, &Manage::setConnected);
    QObject::connect(&manage, &Manage::connectedChanged, &runtime, [&](bool connected) {
        runtime.setAndroidConnected(connected);
        if (connected)
            manage.request(QStringLiteral("info"), {}, [&runtime](bool ok, const QJsonObject &reply) {
                if (ok)
                    runtime.setAndroidVersion(ManageCheck::info(reply).value(QStringLiteral("release")).toString());
            });
    });

    SettingsApi settingsApi(&dbusRoot, bus, &callers, &manage, &runtime,
                            {data + QStringLiteral("/icons"), cache + QStringLiteral("/icons")});

    // Once per login, not on every restart of deckardd.
    QFile loginMark(qEnvironmentVariable("XDG_RUNTIME_DIR", QDir::tempPath()) + QStringLiteral("/deckard-login"));
    if (!loginMark.exists() && loginMark.open(QIODevice::WriteOnly)) {
        loginMark.close();
        QTimer::singleShot(3000, &runtime, &Runtime::atLogin);
    }
    return app.exec();
}
