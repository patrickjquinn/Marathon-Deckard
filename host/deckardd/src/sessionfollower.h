#pragma once

#include <QElapsedTimer>
#include <QObject>

#include <functional>

// Restarts the Waydroid session when the shell comes back after going away:
// Waydroid's hwcomposer aborts when its Wayland server disappears, and
// Android is left without a display until the session restarts.
class SessionFollower : public QObject {
    Q_OBJECT

  public:
    static constexpr int kSettleMs   = 2000;
    static constexpr int kMinGapMs   = 30000;

    explicit SessionFollower(std::function<void()> restart, QObject *parent = nullptr);

  public Q_SLOTS:
    void onNameOwnerChanged(const QString &name, const QString &oldOwner,
                            const QString &newOwner);

  private:
    std::function<void()> m_restart;
    bool                  m_lost = false;
    QElapsedTimer         m_last;
};
