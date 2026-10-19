#include "bootwatchdog.h"

#include <QDateTime>
#include <QFileInfo>
#include <QLoggingCategory>
#include <QProcess>

Q_LOGGING_CATEGORY(lcBoot, "deckard.boot")

BootWatchdog::BootWatchdog(Paths paths, std::function<void()> restart, QObject *parent)
    : QObject(parent)
    , m_paths(std::move(paths))
    , m_restart(std::move(restart)) {
    m_timer.setSingleShot(true);
    connect(&m_timer, &QTimer::timeout, this, &BootWatchdog::check);
}

void BootWatchdog::setActiveSince(qint64 ms) {
    m_active = ms > 0;
    if (!m_active) {
        m_timer.stop();
        return;
    }
    if (ms == m_attempt)
        return;
    m_attempt = ms;
    m_timer.start(int(qBound<qint64>(0, ms + kBootMs - QDateTime::currentMSecsSinceEpoch(), qint64(kBootMs))));
}

bool BootWatchdog::booted() const {
    const QFileInfo marker(m_paths.marker);
    return marker.exists() && marker.lastModified().toMSecsSinceEpoch() >= m_attempt;
}

void BootWatchdog::check() {
    if (booted()) {
        m_restarted = false;
        return;
    }
    if (m_busy)
        return;
    qCWarning(lcBoot) << "Android has not finished booting" << kBootMs / 1000 << "s after its start";
    Q_EMIT stalled();
    if (m_paths.evidence.isEmpty()) {
        m_stalled = m_attempt;
        finish();
        return;
    }
    m_busy    = true;
    m_stalled = m_attempt;
    auto *p   = new QProcess(this);
    p->setProcessChannelMode(QProcess::MergedChannels);
    connect(p, &QProcess::finished, this, [this, p] {
        const QList<QByteArray> lines = p->readAll().split('\n');
        for (const QByteArray &line : lines) {
            if (!line.isEmpty())
                qCWarning(lcBoot).noquote() << QString::fromUtf8(line);
        }
        p->deleteLater();
        finish();
    });
    connect(p, &QProcess::errorOccurred, this, [this, p](QProcess::ProcessError error) {
        if (error != QProcess::FailedToStart)
            return;
        qCWarning(lcBoot) << "cannot run" << m_paths.evidence.first() << p->errorString();
        p->deleteLater();
        finish();
    });
    QTimer::singleShot(kEvidenceMs, p, [p] { p->kill(); });
    p->start(m_paths.evidence.first(), m_paths.evidence.mid(1));
}

void BootWatchdog::finish() {
    m_busy = false;
    if (!m_active || m_attempt != m_stalled || booted())
        return;
    if (m_restarted) {
        qCWarning(lcBoot) << "the restarted session stalled too; leaving it";
        return;
    }
    m_restarted = true;
    qCWarning(lcBoot) << "restarting the Android session";
    m_restart();
}
