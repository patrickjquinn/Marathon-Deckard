#pragma once

#include "validate.h"

#include <QDBusConnection>
#include <QElapsedTimer>
#include <QJsonObject>
#include <QObject>
#include <QVariantMap>

#include <map>
#include <memory>

class Bridge;

// One Android MediaSession as an MPRIS2 player, on its own bus connection so
// that each player owns /org/mpris/MediaPlayer2 under its own name.
class MprisPlayer : public QObject {
    Q_OBJECT

  public:
    MprisPlayer(const Validate::Media &first, const QString &artDir, QObject *parent = nullptr);
    ~MprisPlayer() override;

    bool    isRegistered() const;
    void    update(const Validate::Media &state);

    QString identity() const { return m_state.label; }
    QString desktopEntry() const { return QStringLiteral("waydroid.") + m_state.pkg; }
    QString playbackStatus() const { return m_state.status; }
    double  rate() const { return m_state.speed > 0 ? m_state.speed : 1.0; }
    QVariantMap metadata() const;
    qlonglong position() const;
    bool canPlay() const { return m_state.canPlay; }
    bool canPause() const { return m_state.canPause; }
    bool canNext() const { return m_state.canNext; }
    bool canPrevious() const { return m_state.canPrevious; }
    bool canSeek() const { return m_state.canSeek && m_state.durationMs > 0; }
    QString trackId() const;

  Q_SIGNALS:
    void command(const QString &command, qint64 positionMs);
    void seeked(qlonglong positionUs);

  private:
    void setArt(const QByteArray &png);
    void propertiesChanged(const QString &interface, const QVariantMap &changed);

    QDBusConnection  m_bus;
    QString          m_busName;
    QString          m_artDir;
    QString          m_artPath;
    Validate::Media  m_state;
    QElapsedTimer    m_since;
    int              m_track = 1;
    bool             m_registered = false;
};

class Mpris : public QObject {
    Q_OBJECT

  public:
    static constexpr int kMaxPlayers = 8;

    Mpris(Bridge *bridge, const QString &artDir, QObject *parent = nullptr);

    void handle(const QString &type, const QJsonObject &message);
    void clear();

  private:
    Bridge                                     *m_bridge;
    QString                                     m_artDir;
    std::map<int, std::unique_ptr<MprisPlayer>> m_players;
};
