#include "bridge.h"

#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QJsonDocument>
#include <QLocalSocket>
#include <QLoggingCategory>
#include <QtEndian>

#include <sys/socket.h>

Q_LOGGING_CATEGORY(lcBridge, "deckard.bridge")

namespace {
    qint64 peerUid(const QLocalSocket *socket) {
        ucred     cred{};
        socklen_t length = sizeof(cred);
        if (getsockopt(int(socket->socketDescriptor()), SOL_SOCKET, SO_PEERCRED, &cred, &length) != 0)
            return -1;
        return cred.uid;
    }
} // namespace

Bridge::Bridge(const QString &socketPath, const QString &packagesList, const QByteArray &package,
               QObject *parent)
    : QObject(parent)
    , m_socketPath(socketPath)
    , m_packagesList(packagesList)
    , m_package(package) {
    connect(&m_server, &QLocalServer::newConnection, this, &Bridge::onNewConnection);
    m_refill.start();
}

bool Bridge::listen() {
    const QString dir = QFileInfo(m_socketPath).absolutePath();
    if (!QDir().mkpath(dir))
        return false;
    // The bridge runs under its own Android uid: it must be able to reach the
    // socket. Who may talk is decided by SO_PEERCRED, not file modes.
    QFile::setPermissions(dir, QFile::ReadOwner | QFile::WriteOwner | QFile::ExeOwner |
                                   QFile::ReadGroup | QFile::ExeGroup | QFile::ReadOther |
                                   QFile::ExeOther);
    QLocalServer::removeServer(m_socketPath);
    m_server.setSocketOptions(QLocalServer::WorldAccessOption);
    if (!m_server.listen(m_socketPath)) {
        qCWarning(lcBridge) << "cannot listen on" << m_socketPath << m_server.errorString();
        return false;
    }
    qCInfo(lcBridge) << "listening on" << m_socketPath;
    return true;
}

bool Bridge::isConnected() const {
    return m_client;
}

qint64 Bridge::bridgeUid() const {
    QFile file(m_packagesList);
    if (!file.open(QIODevice::ReadOnly))
        return -1;
    while (!file.atEnd()) {
        const QList<QByteArray> fields = file.readLine().split(' ');
        if (fields.size() >= 2 && fields.at(0) == m_package) {
            bool         ok  = false;
            const qint64 uid = fields.at(1).toLongLong(&ok);
            return ok ? uid : -1;
        }
    }
    return -1;
}

void Bridge::onNewConnection() {
    while (QLocalSocket *socket = m_server.nextPendingConnection()) {
        const qint64 uid      = peerUid(socket);
        const qint64 expected = bridgeUid();
        if (expected < 0 || uid != expected) {
            qCWarning(lcBridge) << m_package << "refused uid" << uid << "expected" << expected;
            socket->abort();
            socket->deleteLater();
            continue;
        }
        if (m_client)
            drop(m_client, QStringLiteral("replaced by a new connection"));
        m_client = socket;
        m_buffer.clear();
        m_tokens  = kBurst;
        m_dropped = 0;
        connect(socket, &QLocalSocket::readyRead, this, &Bridge::onReadyRead);
        connect(socket, &QLocalSocket::disconnected, this, [this, socket] {
            socket->deleteLater();
            if (socket != m_client)
                return;
            m_client = nullptr;
            qCInfo(lcBridge) << m_package << "disconnected";
            Q_EMIT connectedChanged(false);
        });
        qCInfo(lcBridge) << m_package << "connected, uid" << uid;
        Q_EMIT connectedChanged(true);
    }
}

void Bridge::drop(QLocalSocket *socket, const QString &reason) {
    qCWarning(lcBridge) << "dropping" << m_package << reason;
    socket->abort();
}

bool Bridge::takeToken() {
    m_tokens = qMin<double>(kBurst, m_tokens + m_refill.restart() * kPerSecond / 1000.0);
    if (m_tokens < 1)
        return false;
    m_tokens -= 1;
    return true;
}

void Bridge::onReadyRead() {
    QLocalSocket *socket = m_client;
    if (!socket || sender() != socket)
        return;
    m_buffer += socket->readAll();
    while (m_buffer.size() >= 4) {
        const quint32 length = qFromBigEndian<quint32>(m_buffer.constData());
        if (length == 0 || length > quint32(kMaxFrame)) {
            drop(socket, QStringLiteral("frame of %1 bytes").arg(length));
            return;
        }
        if (m_buffer.size() < qsizetype(4 + length))
            return;
        const QByteArray frame = m_buffer.mid(4, length);
        m_buffer.remove(0, 4 + length);

        QJsonParseError     error{};
        const QJsonDocument document = QJsonDocument::fromJson(frame, &error);
        const QString       type     = document.object().value(QStringLiteral("t")).toString();
        if (!document.isObject() || type.isEmpty() || type.size() > 32) {
            drop(socket, QStringLiteral("malformed frame: %1").arg(error.errorString()));
            return;
        }
        if (!takeToken()) {
            if ((++m_dropped & (m_dropped - 1)) == 0)
                qCWarning(lcBridge) << "rate limit:" << m_dropped << "messages dropped";
            if (m_dropped > 1000) {
                drop(socket, QStringLiteral("flooding"));
                return;
            }
            continue;
        }
        Q_EMIT received(type, document.object());
        if (socket != m_client)
            return;
    }
}

void Bridge::send(const QJsonObject &message) {
    if (!m_client)
        return;
    if (m_client->bytesToWrite() > 4 * kMaxFrame) {
        drop(m_client, QStringLiteral("not reading"));
        return;
    }
    const QByteArray body = QJsonDocument(message).toJson(QJsonDocument::Compact);
    char             header[4];
    qToBigEndian<quint32>(quint32(body.size()), header);
    m_client->write(header, 4);
    m_client->write(body);
}
