#pragma once

#include <QDBusConnection>
#include <QDBusContext>
#include <QDBusMessage>
#include <QHash>
#include <QJsonArray>
#include <QJsonObject>
#include <QList>
#include <QObject>
#include <QPointer>
#include <QStringList>
#include <QVariantMap>

#include <functional>
#include <memory>
#include <optional>

class QTimer;

// Android apps as Keel Actions providers (protocol/bridge.md, Keel Actions):
// what DeckardBridge reports about each app, turned into a Keel manifest
// served as org.shipwright.Keel.Actions on the bus name waydroid.<package>.
namespace KeelAndroid {

    struct Link {
        QString scheme;
        QString host;
    };
    struct Shortcut {
        QString id;
        QString label;
        QString longLabel;
    };
    struct App {
        QString         pkg;
        QString         label;
        QList<Link>     links;
        bool            share = false;
        QString         search; // "gms", "android" or ""
        QList<Shortcut> shortcuts;
    };
    std::optional<App> app(const QJsonObject &message, QString *error);
    QList<Shortcut>    shortcuts(const QJsonArray &list);
    QJsonObject        toJson(const App &app);

    QString appId(const QString &pkg);
    // The Keel action a shortcut becomes: "shortcut.<word>".
    QString shortcutAction(const QString &id, const QStringList &taken);
    // Shortcuts that act at once or touch money, messages or data.
    bool    needsConfirmation(const QString &label);

    struct Manifest {
        QJsonObject             json;
        QHash<QString, QString> shortcuts; // action → shortcut id
    };
    Manifest manifest(const App &app);

    // A link app.show may hand to Android: a URI of a scheme apps declare.
    bool linkAllowed(const QString &uri, QString *error);

    enum NodeFlag {
        Clickable     = 1,
        LongClickable = 2,
        Editable      = 4,
        Checkable     = 8,
        Checked       = 16,
        Selected      = 32,
        Disabled      = 64,
        Focused       = 128,
        Scrollable    = 256,
        Password      = 512,
        Heading       = 1024,
        AllFlags      = 2047
    };
    struct Node {
        int     index = 0;
        int     depth = 0;
        QString cls;
        QString text;
        QString desc;
        QString hint;
        QString viewId;
        int     flags = 0;
    };
    struct Screen {
        QString     pkg;
        qint64      snapshot = 0;
        QString     title;
        QList<Node> nodes;
    };
    std::optional<Screen> screen(const QJsonObject &message, QString *error);
    QString               role(const Node &node);
    // One line per node, indented by depth; at most maxChars.
    QString               render(const Screen &screen, int maxChars, bool *truncated);
    // "<snapshot>.<index>" from screen.read.
    bool                  parseNode(const QString &id, qint64 *snapshot, int *index);

    // A paired computer's keel-mcp (--external) gets no screen content.
    bool isExternalHost(const QByteArray &cmdline);

} // namespace KeelAndroid

class KeelApps;

// One Android app's org.shipwright.Keel.Actions, on a connection of its own:
// keel-mcp knows apps by bus name and calls every one on the same path.
class KeelProvider : public QObject, protected QDBusContext {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.shipwright.Keel.Actions")

  public:
    KeelProvider(KeelApps *apps, const QString &pkg, QObject *parent = nullptr);

  public Q_SLOTS:
    QString Describe();
    QString Invoke(const QString &action, const QString &arguments, const QVariantMap &options);
    QString GetEntity(const QString &type, const QString &id);
    QString FindEntities(const QString &type, const QString &query, uint limit);
    QString GetContext();

  private:
    KeelApps *m_apps;
    QString   m_pkg;
};

class KeelApps : public QObject {
    Q_OBJECT

  public:
    using Send    = std::function<void(const QJsonObject &)>;
    using Connect = std::function<QDBusConnection(const QString &connectionName)>;

    static constexpr int kMaxApps          = 200;
    static constexpr int kBridgeTimeoutMs  = 15000;
    static constexpr int kWindowWaitMs     = 15000;
    static constexpr int kScreenChars      = 16000;
    static constexpr int kContextChars     = 4000;

    // send: the bridge (DeckardBridge); system: the system channel
    // (DeckardSystem: shortcuts, secure windows). shell: where
    // org.marathonos.shell and org.marathonos.Shell are. trusted: executables
    // whose calls are served (keel-mcp, pilotd), by file.
    KeelApps(Send send, Send system, Connect connect, QDBusConnection shell, QStringList trusted,
             const QString &statePath, QObject *parent = nullptr);
    ~KeelApps() override;

    static bool handles(const QString &type);
    void        handle(const QString &type, const QJsonObject &message);
    void        setConnected(bool connected);
    static bool handlesSystem(const QString &type);
    void        handleSystem(const QString &type, const QJsonObject &message);
    void        setSystemConnected(bool connected);

    QStringList packages() const;
    QJsonObject manifest(const QString &pkg) const;

  private:
    friend class KeelProvider;

    struct Entry {
        KeelAndroid::App       app;
        KeelAndroid::Manifest  manifest;
        QString                connection;
        QPointer<KeelProvider> provider;
        qint64                 snapshot = 0; // the last screen.read's
    };
    struct Request {
        QJsonObject                                     message;
        bool                                            system = false;
        bool                                            sent = false;
        QTimer                                         *timer = nullptr;
        std::function<void(bool, const QJsonObject &)> done;
    };
    // code: the Keel error code ("NotAvailable", ...) when not ok.
    using ShellDone = std::function<void(bool ok, const QJsonObject &result, const QString &code,
                                         const QString &message)>;
    using Done      = std::function<void(const QString &code, const QString &message)>;

    void    invoke(const QString &pkg, const QString &action, const QJsonObject &args,
                   bool userInitiated, bool external, const QDBusMessage &call,
                   QDBusConnection connection);
    void    context(const QString &pkg, bool external, const QDBusMessage &call,
                    QDBusConnection connection);
    bool    trustedCaller(const QDBusMessage &call, QDBusConnection connection, bool *external) const;

    void    onApp(const QJsonObject &message);
    void    onAppGone(const QString &pkg);
    void    onAppList(const QJsonObject &message);
    void    onReply(const QJsonObject &message);
    void    publish(const KeelAndroid::App &app);
    void    unpublish(const QString &pkg);
    void    save() const;
    void    load();

    void    request(QJsonObject message, std::function<void(bool, const QJsonObject &)> done,
                    bool system = false);
    void    connectedChanged(bool system, bool connected);
    void    refreshShortcuts(const QString &pkg);
    void    notSecure(const QString &pkg, Done done);
    void    shell(const QString &action, const QJsonObject &args, ShellDone done);
    void    unlocked(const QString &pkg, bool inFront, Done done);
    void    launchThenShow(const QString &pkg, const QJsonObject &launch, bool system, Done done);
    void    waitForWindow(const QString &appId, int left, Done done);
    void    expectWindow(const QString &appId, std::function<void(bool expected)> done);
    void    readScreen(const QString &pkg, int maxChars,
                       std::function<void(const QString &code, const QString &message,
                                          const QJsonObject &result)> done);

    Send                       m_send;
    Send                       m_system;
    Connect                    m_connect;
    QDBusConnection            m_shell;
    QStringList                m_trusted;
    QString                    m_statePath;
    bool                       m_connected = false;
    bool                       m_systemConnected = false;
    qint64                     m_nextRequest = 1;
    int                        m_connections = 0;
    QHash<QString, Entry>      m_apps;
    QHash<qint64, Request>     m_requests;
};
