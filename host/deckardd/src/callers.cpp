#include "callers.h"

#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QFile>
#include <QLoggingCategory>

Q_LOGGING_CATEGORY(lcCallers, "deckard.callers")

namespace {
    void callAsync(QDBusConnection bus, const QDBusMessage &call, QObject *context,
                   std::function<void(const QDBusMessage &)> done) {
        auto *watcher = new QDBusPendingCallWatcher(bus.asyncCall(call, 5000), context);
        QObject::connect(watcher, &QDBusPendingCallWatcher::finished, context,
                         [watcher, done = std::move(done)] {
                             watcher->deleteLater();
                             done(watcher->reply());
                         });
    }
} // namespace

Callers::Callers(QDBusConnection bus, QObject *parent)
    : Callers(bus, nullptr, nullptr, &Callers::parentFromProc, parent) {}

Callers::Callers(QDBusConnection bus, PidOf pidOf, Lookup lookup, Parent parentOf, QObject *parent)
    : QObject(parent)
    , m_bus(bus)
    , m_pidOf(std::move(pidOf))
    , m_lookup(std::move(lookup))
    , m_parentOf(std::move(parentOf)) {
    if (!m_pidOf) {
        m_pidOf = [this](const QString &sender, std::function<void(qint64)> done) {
            QDBusMessage call = QDBusMessage::createMethodCall(
                QStringLiteral("org.freedesktop.DBus"), QStringLiteral("/org/freedesktop/DBus"),
                QStringLiteral("org.freedesktop.DBus"), QStringLiteral("GetConnectionUnixProcessID"));
            call << sender;
            callAsync(m_bus, call, this, [done = std::move(done)](const QDBusMessage &reply) {
                const QVariantList args = reply.arguments();
                done(reply.type() == QDBusMessage::ReplyMessage && !args.isEmpty()
                         ? qint64(args.first().toUInt())
                         : -1);
            });
        };
    }
    if (!m_lookup) {
        m_lookup = [this](qint64 pid, AppId done) {
            QDBusMessage call = QDBusMessage::createMethodCall(
                QStringLiteral("org.marathonos.Shell.Session"), QStringLiteral("/org/marathonos/Shell/Navigation"),
                QStringLiteral("org.marathonos.Shell.Navigation1"), QStringLiteral("AppIdForPid"));
            call << uint(pid);
            callAsync(m_bus, call, this, [done = std::move(done)](const QDBusMessage &reply) {
                const QVariantList args = reply.arguments();
                if (reply.type() != QDBusMessage::ReplyMessage)
                    qCWarning(lcCallers) << "AppIdForPid:" << reply.errorMessage();
                done(reply.type() == QDBusMessage::ReplyMessage && !args.isEmpty()
                         ? args.first().toString()
                         : QString());
            });
        };
    }
}

qint64 Callers::parentFromProc(qint64 pid) {
    QFile stat(QStringLiteral("/proc/%1/stat").arg(pid));
    if (!stat.open(QIODevice::ReadOnly))
        return -1;
    const QByteArray line  = stat.readAll();
    const qsizetype  close = line.lastIndexOf(')');
    if (close < 0)
        return -1;
    const QList<QByteArray> fields = line.mid(close + 1).trimmed().split(' ');
    return fields.size() >= 2 ? fields.at(1).toLongLong() : -1;
}

void Callers::resolve(const QString &sender, AppId done) {
    m_pidOf(sender, [this, done = std::move(done)](qint64 pid) {
        if (pid <= 1) {
            done(QString());
            return;
        }
        QList<qint64> chain{pid};
        for (qint64 p = m_parentOf(pid); p > 1 && chain.size() < kMaxHops; p = m_parentOf(p))
            chain.append(p);
        lookupChain(chain, done);
    });
}

void Callers::lookupChain(QList<qint64> chain, AppId done) {
    if (chain.isEmpty()) {
        done(QString());
        return;
    }
    const qint64 pid = chain.takeFirst();
    m_lookup(pid, [this, chain, done](const QString &appId) {
        if (!appId.isEmpty())
            done(appId);
        else
            lookupChain(chain, done);
    });
}

void Callers::require(const QDBusMessage &message, const QStringList &allowed,
                      std::function<void()> run) {
    message.setDelayedReply(true);
    resolve(message.service(), [this, message, allowed, run = std::move(run)](const QString &appId) {
        if (!appId.isEmpty() && allowed.contains(appId)) {
            run();
            return;
        }
        qCWarning(lcCallers) << message.member() << "refused for" << (appId.isEmpty() ? QStringLiteral("<unknown>") : appId);
        m_bus.send(message.createErrorReply(QDBusError::AccessDenied,
                                            QStringLiteral("Not allowed for this caller")));
    });
}
