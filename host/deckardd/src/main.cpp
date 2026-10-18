#include "bridge.h"
#include "mpris.h"
#include "notifications.h"

#include <QCoreApplication>
#include <QDBusConnection>
#include <QDBusError>
#include <QDir>
#include <QLoggingCategory>
#include <QStandardPaths>

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
    Notifications notifications(&bridge, cache + QStringLiteral("/icons"),
                                state + QStringLiteral("/notifications.json"));
    Mpris         mpris(&bridge, cache + QStringLiteral("/art"));

    QObject::connect(&bridge, &Bridge::received, &app,
                     [&](const QString &type, const QJsonObject &message) {
                         if (type.startsWith(QLatin1String("media")))
                             mpris.handle(type, message);
                         else
                             notifications.handle(type, message);
                     });
    QObject::connect(&bridge, &Bridge::connectedChanged, &app, [&](bool connected) {
        if (!connected)
            mpris.clear();
        else
            bridge.send({{QStringLiteral("t"), QStringLiteral("hello")}, {QStringLiteral("v"), 1}});
    });
    if (!bridge.listen())
        return 1;
    return app.exec();
}
