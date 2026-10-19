#pragma once

#include <QDBusAbstractAdaptor>
#include <QDBusConnection>
#include <QDBusMessage>
#include <QHash>
#include <QJsonObject>
#include <QList>
#include <QObject>
#include <QVariantMap>

#include <functional>

using ShareTargetList = QList<QVariantMap>;

// Android apps in Marathon's share sheet (protocol/bridge.md, Share). As
// targets: the shell asks org.marathonos.Deckard1.Share for the activities
// that take a type and hands the chosen one the request, whose files are
// copied into xfer/ for DeckardSystem's provider. As sources: DeckardSystem
// answers Android's chooser with share_out, which becomes a share(a{sv}) on
// org.sailfishos.share with the files as descriptors.
class Sharing : public QDBusAbstractAdaptor {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.marathonos.Deckard1.Share")

  public:
    // Sends on the system channel; false while DeckardSystem isn't connected.
    using Send    = std::function<bool(const QJsonObject &)>;
    using IsShell = std::function<bool(const QDBusMessage &)>;

    static constexpr int    kMaxResources = 32;
    static constexpr int    kMaxText      = 65536;
    static constexpr int    kMaxTargets   = 64;
    static constexpr qint64 kMaxBytes     = Q_INT64_C(1) << 30;
    static constexpr int    kTimeoutMs    = 15000;

    // root: the object exported at /org/marathonos/Deckard.
    Sharing(QObject *root, QDBusConnection bus, Send send, IsShell isShell, const QString &xferDir);

    // "<pkg>/<activity class>", at most 256 characters.
    static bool isTargetId(const QString &id);
    // A D-Bus a{sv} as plain maps, lists and descriptors.
    static QVariant plain(const QVariant &value);

    void handle(const QString &type, const QJsonObject &message);
    void systemLost();

  public Q_SLOTS:
    // {id, pkg, label, icon (data: URI), multiple} for the activities taking
    // ACTION_SEND of mimeType; multiple: they take ACTION_SEND_MULTIPLE too.
    ShareTargetList ShareTargets(const QString &mimeType, const QDBusMessage &call);
    // The sheet's request (mimeType, title, resources) for target id.
    void Share(const QString &id, const QVariantMap &configuration, const QDBusMessage &call);

  protected:
    virtual void sendReply(const QDBusMessage &reply);

  private:
    struct Pending {
        QDBusMessage call;
        QString      type; // "targets" or "share"
    };

    bool    refuse(const QDBusMessage &call);
    void    start(const QString &id, const QJsonObject &message, const Pending &pending);
    void    fail(const QString &id, const QString &message);
    QString copyIn(const QString &tid, const QVariantList &resources, QStringList *files,
                   QString *text, qint64 *bytes);
    void    shareOut(const QJsonObject &message);
    void    targetsResult(const QString &id, const QJsonObject &message);

    QDBusConnection         m_bus;
    Send                    m_send;
    IsShell                 m_isShell;
    QString                 m_xfer;
    QHash<QString, Pending> m_pending;
};
