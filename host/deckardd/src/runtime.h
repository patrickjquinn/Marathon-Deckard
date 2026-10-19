#pragma once

#include <QDBusConnection>
#include <QObject>
#include <QStringList>
#include <QVariantMap>

#include <functional>

// The Android session (marathon-waydroid.service) as Settings shows it:
// stopped, starting, running or stopping; its memory, Android version and
// uptime; and whether it starts at login.
class Runtime : public QObject {
    Q_OBJECT

  public:
    struct Paths {
        QString memory;       // the container cgroup's memory.current
        QString policy;       // deckardd's settings, JSON
        QString applications; // where Waydroid writes waydroid.<pkg>.desktop
        // The root helper that removes Android's data, as a command; by default
        // pkexec /usr/libexec/deckard-reset, or $DECKARD_RESET_HELPER.
        QStringList reset;
    };
    // StartUnit, StopUnit or RestartUnit of the session unit.
    using UnitCall = std::function<void(const QString &method)>;
    using Done     = std::function<void(bool ok, const QString &error)>;

    static constexpr int kStopTimeoutMs = 90000;

    explicit Runtime(Paths paths, UnitCall unitCall, QObject *parent = nullptr);

    QString     state() const;
    QVariantMap snapshot() const;
    bool        startAtLogin() const;
    bool        startAtLoginIsSet() const;
    bool        hasAndroidApps() const;

    void start();
    void stop();
    void restart();
    void setStartAtLogin(bool on);
    void atLogin();
    // Stops Android, then runs the reset helper; the next start sets Android up again.
    void reset(Done done);

  public Q_SLOTS:
    // systemd's ActiveState and ActiveEnterTimestamp (ms since the epoch).
    void setUnit(const QString &activeState, qint64 enteredMs);
    void setAndroidConnected(bool connected);
    void setAndroidVersion(const QString &release);

  Q_SIGNALS:
    void changed();

  private:
    Paths    m_paths;
    UnitCall m_unitCall;
    QString  m_unitState = QStringLiteral("inactive");
    qint64   m_enteredMs = 0;
    bool     m_android   = false;
    QString  m_version;
    bool     m_resetting = false;

    void runReset(Done done);
};

// Follows marathon-waydroid.service through the user's systemd and feeds
// Runtime::setUnit.
class UnitWatch : public QObject {
    Q_OBJECT

  public:
    UnitWatch(QDBusConnection bus, const QString &unit, Runtime *runtime, QObject *parent = nullptr);

    static void call(QDBusConnection bus, const QString &method, const QString &unit);

  private Q_SLOTS:
    void onPropertiesChanged(const QString &interface, const QVariantMap &changed,
                             const QStringList &invalidated);

  private:
    void refresh();

    QDBusConnection m_bus;
    QString         m_unit;
    QString         m_path;
    Runtime        *m_runtime;
};
