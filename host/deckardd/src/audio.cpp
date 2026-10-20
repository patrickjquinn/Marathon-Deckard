#include "audio.h"

#include "bridge.h"

#include <QDBusConnection>
#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QLoggingCategory>

Q_LOGGING_CATEGORY(lcAudio, "deckard.audio")

namespace {
    const QString kShell = QStringLiteral("org.marathonos.Shell.Session");
} // namespace

AudioSync::AudioSync(Bridge *bridge, QObject *parent)
    : QObject(parent)
    , m_bridge(bridge) {
    QDBusConnection bus = QDBusConnection::sessionBus();
    bus.connect(kShell, QStringLiteral("/org/marathonos/Shell/Audio"),
                QStringLiteral("org.marathonos.Shell.Audio1"), QStringLiteral("StateChanged"), this,
                SLOT(refresh()));
    bus.connect(kShell, QStringLiteral("/org/marathonos/Shell/Settings"),
                QStringLiteral("org.marathonos.Shell.Settings1"), QStringLiteral("PropertyChanged"),
                this, SLOT(onSettingChanged(QString)));
    bus.connect(QStringLiteral("org.freedesktop.DBus"), QStringLiteral("/org/freedesktop/DBus"),
                QStringLiteral("org.freedesktop.DBus"), QStringLiteral("NameOwnerChanged"), this,
                SLOT(onSettingChanged(QString)));
    refresh();
}

void AudioSync::onSettingChanged(const QString &name) {
    if (name == QLatin1String("dndEnabled") || name == QLatin1String("audioProfile") ||
        name == QLatin1String("alarmVolume") || name == kShell)
        refresh();
}

void AudioSync::refresh() {
    const QDBusMessage call = QDBusMessage::createMethodCall(
        kShell, QStringLiteral("/org/marathonos/Shell/Settings"),
        QStringLiteral("org.marathonos.Shell.Settings1"), QStringLiteral("GetState"));
    auto *watcher = new QDBusPendingCallWatcher(QDBusConnection::sessionBus().asyncCall(call), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, watcher] {
        watcher->deleteLater();
        const QDBusPendingReply<QVariantMap> reply = *watcher;
        if (reply.isError()) {
            qCWarning(lcAudio) << "cannot read Marathon's audio settings:" << reply.error().message();
            return;
        }
        const QVariantMap state   = reply.value();
        const QString     profile = state.value(QStringLiteral("audioProfile")).toString();
        const bool        quiet   = state.value(QStringLiteral("dndEnabled")).toBool() ||
                           profile == QLatin1String("silent") || profile == QLatin1String("vibrate");
        const QJsonObject message{
            {QStringLiteral("t"), QStringLiteral("audio")},
            {QStringLiteral("quiet"), quiet},
            {QStringLiteral("alarm"),
             qBound(0.0, state.value(QStringLiteral("alarmVolume"), 0.9).toDouble(), 1.0)}};
        if (message == m_last)
            return;
        m_last = message;
        qCInfo(lcAudio) << "Marathon is" << (quiet ? "quiet" : "audible") << "(profile" << profile
                        << ")";
        m_bridge->send(m_last);
    });
}

void AudioSync::resend() {
    if (m_last.isEmpty())
        refresh();
    else
        m_bridge->send(m_last);
}
