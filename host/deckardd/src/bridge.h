#pragma once

#include <QElapsedTimer>
#include <QJsonObject>
#include <QLocalServer>
#include <QPointer>

class QLocalSocket;

// A socket a Deckard app connects to from inside the container. Only that
// app's Android uid (from packages.list) is accepted, one connection
// at a time; frames are length-prefixed JSON objects with a "t" type, size-
// and rate-limited (protocol/bridge.md).
class Bridge : public QObject {
    Q_OBJECT

  public:
    static constexpr int kMaxFrame  = 1 << 20;
    static constexpr int kBurst     = 200;
    static constexpr int kPerSecond = 50;

    Bridge(const QString &socketPath, const QString &packagesList,
           const QByteArray &package = QByteArrayLiteral("org.marathonos.deckard.bridge"),
           QObject *parent = nullptr);

    bool listen();
    bool isConnected() const;
    void send(const QJsonObject &message);

  Q_SIGNALS:
    void connectedChanged(bool connected);
    void received(const QString &type, const QJsonObject &message);

  private:
    void onNewConnection();
    void onReadyRead();
    void drop(QLocalSocket *socket, const QString &reason);
    bool takeToken();
    qint64 bridgeUid() const;

    QString                m_socketPath;
    QString                m_packagesList;
    QByteArray             m_package;
    QLocalServer           m_server;
    QPointer<QLocalSocket> m_client;
    QByteArray             m_buffer;
    double                 m_tokens = kBurst;
    QElapsedTimer          m_refill;
    int                    m_dropped = 0;
};
