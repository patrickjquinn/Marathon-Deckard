#pragma once

#include <QElapsedTimer>
#include <QFileSystemWatcher>
#include <QJsonObject>
#include <QObject>
#include <QSocketNotifier>

#include <functional>
#include <memory>
#include <vector>

// Asks DeckardSystem to end Android's least recently used cached processes
// when memory runs short, the job lmkd cannot do on this kernel: the
// container going over memory.high or memory.max (memory.events), Android
// stalling on memory inside the container, or the whole phone stalling at
// the level where the shell starts trimming its own apps.
class Reclaimer : public QObject {
    Q_OBJECT

  public:
    struct Paths {
        QString cgroup;
        QString systemPressure = QStringLiteral("/proc/pressure/memory");
    };
    // Unprivileged PSI triggers need a window that is a multiple of 2 s.
    static constexpr const char *kContainerTrigger = "some 200000 2000000";
    static constexpr const char *kSystemTrigger    = "some 400000 2000000";
    static constexpr int         kCount            = 2;
    static constexpr int         kGapMs            = 2000;
    static constexpr int         kIdleGapMs        = 10000;

    Reclaimer(Paths paths, std::function<void(const QJsonObject &)> send, QObject *parent = nullptr);
    ~Reclaimer() override;

    // memory.events' counters that mean the container is over its budget.
    static quint64 overBudget(const QByteArray &events);

  public Q_SLOTS:
    void setConnected(bool connected);
    void handle(const QString &type, const QJsonObject &message);

  Q_SIGNALS:
    void requested(const QString &reason);

  private:
    struct Trigger {
        int                              fd = -1;
        std::unique_ptr<QSocketNotifier> notifier;
    };

    void arm();
    void disarm();
    void watchTrigger(const QString &path, const char *spec, const QString &reason);
    void eventsChanged();
    void request(const QString &reason);

    Paths                                     m_paths;
    std::function<void(const QJsonObject &)> m_send;
    QFileSystemWatcher                        m_events;
    std::vector<Trigger>                      m_triggers;
    quint64                                   m_over      = 0;
    bool                                      m_connected = false;
    bool                                      m_idle      = false;
    int                                       m_serial    = 0;
    QElapsedTimer                             m_last;
};
