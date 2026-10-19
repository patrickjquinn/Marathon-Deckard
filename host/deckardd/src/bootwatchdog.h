#pragma once

#include <QObject>
#include <QStringList>
#include <QTimer>

#include <functional>

// Restarts an Android session that never finishes booting. Waydroid's
// composer HAL can connect to Wayland and never register, leaving
// SurfaceFlinger and then system_server waiting for good; a second start
// works. If /data/deckard/booted (written at sys.boot_completed) is not newer
// than the session's start kBootMs later, the evidence helper's output goes
// to the journal and the session restarts. A restart that also stalls is
// only reported.
class BootWatchdog : public QObject {
    Q_OBJECT

  public:
    static constexpr int kBootMs     = 60000;
    static constexpr int kEvidenceMs = 90000;

    struct Paths {
        QString     marker;
        QStringList evidence; // a command; empty to skip
    };

    BootWatchdog(Paths paths, std::function<void()> restart, QObject *parent = nullptr);

  public Q_SLOTS:
    // The session unit's activation time (ms since the epoch), 0 while it isn't active.
    void setActiveSince(qint64 ms);

  Q_SIGNALS:
    void stalled();

  private:
    void check();
    void finish();
    bool booted() const;

    Paths                 m_paths;
    std::function<void()> m_restart;
    QTimer                m_timer;
    qint64                m_attempt   = 0;
    qint64                m_stalled   = 0;
    bool                  m_active    = false;
    bool                  m_restarted = false;
    bool                  m_busy      = false;
};
