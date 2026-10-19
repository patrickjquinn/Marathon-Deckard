#pragma once

#include <QDBusConnection>
#include <QObject>
#include <QStringList>

#include <functional>

class QDBusMessage;

// Who calls deckardd over D-Bus: the Marathon app id the shell gives the
// sender's process or its nearest ancestor (Navigation1.AppIdForPid, which
// answers declared session services such as deckardd), as the shell's own
// services resolve their callers.
class Callers : public QObject {
    Q_OBJECT

  public:
    using AppId  = std::function<void(const QString &appId)>;
    using PidOf  = std::function<void(const QString &sender, std::function<void(qint64)>)>;
    using Lookup = std::function<void(qint64 pid, AppId)>;
    using Parent = std::function<qint64(qint64 pid)>;

    static constexpr int kMaxHops = 8;

    explicit Callers(QDBusConnection bus, QObject *parent = nullptr);
    Callers(QDBusConnection bus, PidOf pidOf, Lookup lookup, Parent parentOf,
            QObject *parent = nullptr);

    // done gets "" when the sender is no Marathon app.
    void resolve(const QString &sender, AppId done);

    // Runs run once the caller of message is one of allowed; otherwise
    // replies AccessDenied. Either way the reply to message is delayed: run
    // sends it.
    void require(const QDBusMessage &message, const QStringList &allowed,
                 std::function<void()> run);

    static qint64 parentFromProc(qint64 pid);

  private:
    void lookupChain(QList<qint64> chain, AppId done);

    QDBusConnection m_bus;
    PidOf           m_pidOf;
    Lookup          m_lookup;
    Parent          m_parentOf;
};
