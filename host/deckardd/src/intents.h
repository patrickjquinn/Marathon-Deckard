#pragma once

#include <QDBusConnection>
#include <QHash>
#include <QJsonObject>
#include <QObject>
#include <QStringList>

#include <functional>
#include <optional>

class QDBusMessage;

// Android intents that Marathon apps answer (protocol/bridge.md, system
// channel): links, numbers, addresses, places and settings pages from
// DeckardSystem's stubs, and its file pickers and camera requests.
namespace Intents {

    // Where an "open" goes: a shell route (Navigation1.LaunchAppWithRoute), a
    // Keel action of a Marathon app, or just the app (Navigation1.LaunchApp).
    struct Target {
        enum Kind { Route, Keel, Launch };
        Kind        kind = Launch;
        QString     app;  // Marathon app id: "browser", "phone", ...
        QString     name; // route or Keel action
        QJsonObject args;
    };
    std::optional<Target> target(const QJsonObject &open, QString *error);

    QString settingsRoute(const QString &androidAction, const QString &pkg);
    bool    isRequestId(const QString &id);
    QString safeName(const QString &name);

} // namespace Intents

class SystemIntents : public QObject {
    Q_OBJECT

  public:
    using Send = std::function<void(const QJsonObject &)>;

    SystemIntents(Send send, const QString &xferDir, const QString &cameraDir,
                  QDBusConnection bus, QObject *parent = nullptr);

    void handle(const QString &type, const QJsonObject &message);
    void cancelAll();
    void resendFocus();

  private Q_SLOTS:
    void onPortalResponse(const QDBusMessage &message);
    void onForegroundApp(const QString &appId);

  private:
    struct Pending {
        QString     mode;
        QString     kind;
        QString     portalPath;
        QString     saveTarget; // the host file a "save" writes to
    };

    void open(const QJsonObject &message);
    void pick(const QJsonObject &message);
    void pickDone(const QString &id);
    void finish(const QString &id, const QStringList &hostFiles, bool ok);
    void capture(const QString &id, const QString &kind);
    void drop(const QString &id);
    void sweep();
    void callLogged(const QDBusMessage &call);

    Send                    m_send;
    QString                 m_xfer;
    QString                 m_cameraDir;
    QDBusConnection         m_bus;
    QHash<QString, Pending> m_pending;
    QHash<QString, QString> m_saved; // id → host target, kept for later writes
    QString                 m_focusPkg; // Android app Marathon has in front
};
