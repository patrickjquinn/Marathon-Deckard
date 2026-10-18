#include "doze.h"

#include <QDBusConnection>
#include <QFile>
#include <QLoggingCategory>
#include <QSaveFile>

Q_LOGGING_CATEGORY(lcDoze, "deckard.doze")

DozeSync::DozeSync(const QString &path, QObject *parent)
    : QObject(parent)
    , m_path(path) {
    QDBusConnection::sessionBus().connect(
        QStringLiteral("org.marathonos.Shell"), QStringLiteral("/org/marathonos/Shell/Power"),
        QStringLiteral("org.marathonos.Shell.Power1"), QStringLiteral("StateChanged"), this,
        SLOT(onPowerState(QVariantMap)));
}

void DozeSync::onPowerState(const QVariantMap &state) {
    const auto it = state.constFind(QStringLiteral("dozing"));
    if (it == state.constEnd())
        return;
    const QByteArray content = it->toBool() ? "1\n" : "0\n";
    QFile current(m_path);
    if (current.open(QIODevice::ReadOnly) && current.readAll() == content)
        return;
    current.close();
    QSaveFile file(m_path);
    if (!file.open(QIODevice::WriteOnly) || file.write(content) != content.size() || !file.commit()) {
        qCWarning(lcDoze) << "cannot write" << m_path;
        return;
    }
    qCInfo(lcDoze) << (it->toBool() ? "Android idles with Marathon" : "Android wakes with Marathon");
}
