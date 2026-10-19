#pragma once

#include <QDBusConnection>
#include <QHash>
#include <QJsonObject>
#include <QObject>
#include <QVariantMap>

#include <functional>

// Android's runtime permission requests as Marathon's prompt
// (docs/permissions.md). DeckardSystem sends perm_ask for each request
// PermissionController would show; this asks the shell's
// Permissions1.PromptAndroid and sends perm_answer with the person's choice.
// "unsupported" hands a request back to Android's own dialog.
class PermissionPrompts : public QObject {
    Q_OBJECT

  public:
    using Send = std::function<void(const QJsonObject &)>;

    static constexpr int kMaxWaiting = 8;
    static constexpr int kTimeoutMs  = 10 * 60 * 1000;

    PermissionPrompts(QDBusConnection bus, Send send, QObject *parent = nullptr);

    // The shell's arguments for a perm_ask; false if Marathon can't draw it.
    static bool prompt(const QJsonObject &ask, QString *appId, QString *permission, QVariantMap *options);

    void handle(const QString &type, const QJsonObject &message);
    void systemLost();

  private:
    void answer(const QString &id, const QString &answer, bool precise = false);
    void cancel(const QString &id);

    QDBusConnection     m_bus;
    Send                m_send;
    QHash<QString, int> m_waiting; // id -> generation, so a late reply finds no one
    int                 m_generation = 0;
};
