#include "reclaimer.h"

#include <QFile>
#include <QJsonArray>
#include <QLoggingCategory>

#include <fcntl.h>
#include <unistd.h>

#include <cstring>

Q_LOGGING_CATEGORY(lcReclaim, "deckard.reclaim")

Reclaimer::Reclaimer(Paths paths, std::function<void(const QJsonObject &)> send, QObject *parent)
    : QObject(parent)
    , m_paths(std::move(paths))
    , m_send(std::move(send)) {
    connect(&m_events, &QFileSystemWatcher::fileChanged, this, &Reclaimer::eventsChanged);
}

Reclaimer::~Reclaimer() {
    disarm();
}

quint64 Reclaimer::overBudget(const QByteArray &events) {
    quint64 total = 0;
    for (const QByteArray &line : events.split('\n')) {
        const QList<QByteArray> kv = line.split(' ');
        if (kv.size() == 2 && (kv[0] == "high" || kv[0] == "max"))
            total += kv[1].toULongLong();
    }
    return total;
}

void Reclaimer::setConnected(bool connected) {
    if (connected == m_connected)
        return;
    m_connected = connected;
    if (connected)
        arm();
    else
        disarm();
}

// The container's cgroup is new on every Android start, and DeckardSystem
// connects once Android is up, so each connection arms afresh.
void Reclaimer::arm() {
    disarm();
    const QString events = m_paths.cgroup + QStringLiteral("/memory.events");
    QFile f(events);
    if (f.open(QIODevice::ReadOnly)) {
        m_over = overBudget(f.readAll());
        m_events.addPath(events);
    } else {
        qCWarning(lcReclaim) << "cannot read" << events << f.errorString();
    }
    watchTrigger(m_paths.cgroup + QStringLiteral("/memory.pressure"), kContainerTrigger,
                 QStringLiteral("container-stall"));
    watchTrigger(m_paths.systemPressure, kSystemTrigger, QStringLiteral("phone-stall"));
}

void Reclaimer::disarm() {
    if (!m_events.files().isEmpty())
        m_events.removePaths(m_events.files());
    for (Trigger &t : m_triggers) {
        t.notifier.reset();
        ::close(t.fd);
    }
    m_triggers.clear();
}

void Reclaimer::watchTrigger(const QString &path, const char *spec, const QString &reason) {
    const int fd = ::open(QFile::encodeName(path).constData(), O_RDWR | O_NONBLOCK | O_CLOEXEC);
    if (fd < 0 || ::write(fd, spec, std::strlen(spec) + 1) < 0) {
        qCWarning(lcReclaim) << "no pressure trigger on" << path << std::strerror(errno);
        if (fd >= 0)
            ::close(fd);
        return;
    }
    Trigger t;
    t.fd       = fd;
    t.notifier = std::make_unique<QSocketNotifier>(fd, QSocketNotifier::Exception);
    connect(t.notifier.get(), &QSocketNotifier::activated, this, [this, reason] { request(reason); });
    m_triggers.push_back(std::move(t));
}

void Reclaimer::eventsChanged() {
    QFile f(m_paths.cgroup + QStringLiteral("/memory.events"));
    if (!f.open(QIODevice::ReadOnly))
        return;
    const quint64 over = overBudget(f.readAll());
    if (over > m_over)
        request(QStringLiteral("container-high"));
    m_over = over;
}

void Reclaimer::request(const QString &reason) {
    if (!m_connected)
        return;
    if (m_last.isValid() && m_last.elapsed() < (m_idle ? kIdleGapMs : kGapMs))
        return;
    m_last.start();
    m_send(QJsonObject{{QStringLiteral("t"), QStringLiteral("reclaim")},
                       {QStringLiteral("id"), QString::number(++m_serial)},
                       {QStringLiteral("count"), kCount},
                       {QStringLiteral("reason"), reason}});
    Q_EMIT requested(reason);
}

void Reclaimer::handle(const QString &type, const QJsonObject &message) {
    if (type != QLatin1String("reclaimed"))
        return;
    const QJsonArray killed = message.value(QStringLiteral("killed")).toArray();
    m_idle = message.value(QStringLiteral("ok")).toBool() && killed.isEmpty();
    for (const QJsonValue &k : killed)
        qCInfo(lcReclaim) << "ended" << k.toObject().value(QStringLiteral("process")).toString();
    if (!message.value(QStringLiteral("ok")).toBool())
        qCWarning(lcReclaim) << "reclaim failed:" << message.value(QStringLiteral("error")).toString();
}
