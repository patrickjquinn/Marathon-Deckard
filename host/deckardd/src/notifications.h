#pragma once

#include <QDateTime>
#include <QHash>
#include <QJsonObject>
#include <QObject>
#include <QSet>

class Bridge;

// Android notifications in Marathon's Hub: posts, updates and removes them on
// org.freedesktop.Notifications and turns the Hub's taps, actions, replies
// and dismissals into bridge messages.
class Notifications : public QObject {
    Q_OBJECT

  public:
    static constexpr int kMaxTracked = 200;

    Notifications(Bridge *bridge, const QString &iconDir, const QString &stateFile,
                  QObject *parent = nullptr);

    void handle(const QString &type, const QJsonObject &message);

  private Q_SLOTS:
    void onActionInvoked(uint id, const QString &action);
    void onNotificationClosed(uint id, uint reason);
    void onNotificationReplied(uint id, const QString &text);

  private:
    struct Entry {
        uint       id = 0;
        QByteArray digest;
        QString    replyAction;
        bool       autoCancel = false;
        qint64     invokedAt  = 0;
    };

    void post(const QJsonObject &message);
    void remove(const QString &key);
    void sync(const QJsonObject &message);
    void icon(const QJsonObject &message);
    void close(uint id);
    void load();
    void save() const;

    Bridge               *m_bridge;
    QString               m_iconDir;
    QString               m_stateFile;
    QHash<QString, Entry> m_entries;
    QHash<uint, QString>  m_keys;
};
