#pragma once

#include <QDBusConnection>
#include <QDBusServiceWatcher>
#include <QHash>
#include <QJsonObject>
#include <QObject>
#include <QPair>
#include <QVariantList>

#include <functional>

// Marathon's keyboard in Android apps: DeckardIme reports the focused editor
// over the IME channel (protocol/bridge.md) and this passes it to the shell's
// org.marathonos.Shell.TextInput1, and the shell's typing back.
class Ime : public QObject {
    Q_OBJECT

  public:
    // The app id prefix of Android windows (Waydroid's xdg app ids).
    static inline const QString kAppIdPrefix = QStringLiteral("waydroid.");
    static constexpr int        kMaxText     = 4096;

    using Send = std::function<void(const QJsonObject &)>;
    explicit Ime(Send toAndroid, QObject *parent = nullptr);

    void handle(const QString &type, const QJsonObject &message);
    void setConnected(bool connected);

    // From the shell.
    void commitString(const QString &appId, const QString &text, int before, int after);
    void key(const QString &appId, const QString &name);
    void panelChanged(const QString &appId, int covered, int height);

  Q_SIGNALS:
    // A TextInput1 method call for the shell.
    void shellCall(const QString &method, const QVariantList &arguments);

  private:
    void sendPanel();

    Send                             m_toAndroid;
    QString                          m_appId;
    QHash<QString, QPair<int, int>>  m_panels;
};

// The D-Bus side: registers with the shell (again whenever it restarts) and
// connects its targeted signals to an Ime.
class ShellTextInput : public QObject {
    Q_OBJECT

  public:
    ShellTextInput(Ime *ime, const QDBusConnection &bus, QObject *parent = nullptr);

  private Q_SLOTS:
    void onCommitString(const QString &appId, const QString &text, int before, int after);
    void onKey(const QString &appId, const QString &name);
    void onPanelChanged(const QString &appId, int covered, int height);

  private:
    void registerWithShell();
    void call(const QString &method, const QVariantList &arguments);

    Ime                *m_ime;
    QDBusConnection     m_bus;
    QDBusServiceWatcher m_watcher;
};
