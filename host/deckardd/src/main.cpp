#include "audio.h"
#include "bridge.h"
#include "doze.h"
#include "ime.h"
#include "intents.h"
#include "mpris.h"
#include "notifications.h"
#include "palette.h"

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
    AudioSync     audio(&bridge);
    AmbiencePalette palette(data + QStringLiteral("/deckard/palette"));
    DozeSync      doze(data + QStringLiteral("/deckard/doze"));

    QObject::connect(&bridge, &Bridge::received, &app,
                     [&](const QString &type, const QJsonObject &message) {
                         if (type.startsWith(QLatin1String("media")))
                             mpris.handle(type, message);
                         else
                             notifications.handle(type, message);
                     });
    QObject::connect(&bridge, &Bridge::connectedChanged, &app, [&](bool connected) {
        if (!connected) {
            mpris.clear();
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

    // DeckardSystem runs as Android's system uid, which is also this user's.
    Bridge        systemBridge(data + QStringLiteral("/deckard/system.sock"),
                               data + QStringLiteral("/system/packages.list"),
                               QByteArrayLiteral("org.marathonos.deckard.system"));
    SystemIntents intents([&systemBridge](const QJsonObject &message) { systemBridge.send(message); },
                          data + QStringLiteral("/deckard/xfer"),
                          QStandardPaths::writableLocation(QStandardPaths::PicturesLocation) +
                              QStringLiteral("/Marathon"),
                          bus);
    QObject::connect(&systemBridge, &Bridge::received, &intents, &SystemIntents::handle);
    QObject::connect(&systemBridge, &Bridge::connectedChanged, &intents, [&](bool connected) {
        if (!connected)
            intents.cancelAll();
    });
    if (!systemBridge.listen())
        return 1;
    return app.exec();
}
