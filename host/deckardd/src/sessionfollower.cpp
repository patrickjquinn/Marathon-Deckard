#include "sessionfollower.h"

#include <QDBusConnection>
#include <QLoggingCategory>
#include <QTimer>

Q_LOGGING_CATEGORY(lcFollow, "deckard.session")

SessionFollower::SessionFollower(std::function<void()> restart, QObject *parent)
    : QObject(parent)
    , m_restart(std::move(restart)) {
    QDBusConnection::sessionBus().connect(
        QStringLiteral("org.freedesktop.DBus"), QStringLiteral("/org/freedesktop/DBus"),
        QStringLiteral("org.freedesktop.DBus"), QStringLiteral("NameOwnerChanged"), this,
        SLOT(onNameOwnerChanged(QString, QString, QString)));
}

void SessionFollower::onNameOwnerChanged(const QString &name, const QString &,
                                         const QString &newOwner) {
    if (name != QLatin1String("org.marathonos.Shell"))
        return;
    if (newOwner.isEmpty()) {
        m_lost = true;
        return;
    }
    if (!m_lost)
        return;
    m_lost = false;
    if (m_last.isValid() && m_last.elapsed() < kMinGapMs) {
        qCWarning(lcFollow) << "the shell is restarting repeatedly; not restarting Android again";
        return;
    }
    m_last.start();
    // The compositor is up before the shell's bus name; give it a moment.
    QTimer::singleShot(kSettleMs, this, [this] {
        qCInfo(lcFollow) << "the shell restarted; restarting the Waydroid session";
        m_restart();
    });
}
