#include "mpris.h"

#include "bridge.h"
#include "images.h"

#include <QDBusAbstractAdaptor>
#include <QDBusMessage>
#include <QDBusObjectPath>
#include <QDir>
#include <QFile>
#include <QLoggingCategory>

Q_LOGGING_CATEGORY(lcMpris, "deckard.mpris")

namespace {
    const QString kPath   = QStringLiteral("/org/mpris/MediaPlayer2");
    const QString kRoot   = QStringLiteral("org.mpris.MediaPlayer2");
    const QString kPlayer = QStringLiteral("org.mpris.MediaPlayer2.Player");
    int           gConnections = 0;
} // namespace

class MprisRootAdaptor : public QDBusAbstractAdaptor {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.mpris.MediaPlayer2")
    Q_PROPERTY(bool CanQuit READ no)
    Q_PROPERTY(bool CanRaise READ no)
    Q_PROPERTY(bool HasTrackList READ no)
    Q_PROPERTY(QString Identity READ identity)
    Q_PROPERTY(QString DesktopEntry READ desktopEntry)
    Q_PROPERTY(QStringList SupportedUriSchemes READ none)
    Q_PROPERTY(QStringList SupportedMimeTypes READ none)

  public:
    explicit MprisRootAdaptor(MprisPlayer *player)
        : QDBusAbstractAdaptor(player)
        , m_player(player) {}
    bool        no() const { return false; }
    QString     identity() const { return m_player->identity(); }
    QString     desktopEntry() const { return m_player->desktopEntry(); }
    QStringList none() const { return {}; }

  public Q_SLOTS:
    void Raise() {}
    void Quit() {}

  private:
    MprisPlayer *m_player;
};

class MprisPlayerAdaptor : public QDBusAbstractAdaptor {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.mpris.MediaPlayer2.Player")
    Q_PROPERTY(QString PlaybackStatus READ playbackStatus)
    Q_PROPERTY(double Rate READ rate WRITE setIgnored)
    Q_PROPERTY(QVariantMap Metadata READ metadata)
    Q_PROPERTY(double Volume READ volume WRITE setIgnored)
    Q_PROPERTY(qlonglong Position READ position)
    Q_PROPERTY(double MinimumRate READ rate)
    Q_PROPERTY(double MaximumRate READ rate)
    Q_PROPERTY(bool CanGoNext READ canNext)
    Q_PROPERTY(bool CanGoPrevious READ canPrevious)
    Q_PROPERTY(bool CanPlay READ canPlay)
    Q_PROPERTY(bool CanPause READ canPause)
    Q_PROPERTY(bool CanSeek READ canSeek)
    Q_PROPERTY(bool CanControl READ yes)

  public:
    explicit MprisPlayerAdaptor(MprisPlayer *player)
        : QDBusAbstractAdaptor(player)
        , m_player(player) {
        connect(player, &MprisPlayer::seeked, this, &MprisPlayerAdaptor::Seeked);
    }
    QString     playbackStatus() const { return m_player->playbackStatus(); }
    double      rate() const { return m_player->rate(); }
    double      volume() const { return 1.0; }
    void        setIgnored(double) {}
    QVariantMap metadata() const { return m_player->metadata(); }
    qlonglong   position() const { return m_player->position(); }
    bool        canNext() const { return m_player->canNext(); }
    bool        canPrevious() const { return m_player->canPrevious(); }
    bool        canPlay() const { return m_player->canPlay(); }
    bool        canPause() const { return m_player->canPause(); }
    bool        canSeek() const { return m_player->canSeek(); }
    bool        yes() const { return true; }

  public Q_SLOTS:
    void Next() { send(QStringLiteral("next")); }
    void Previous() { send(QStringLiteral("previous")); }
    void Pause() { send(QStringLiteral("pause")); }
    void PlayPause() { send(QStringLiteral("playpause")); }
    void Stop() { send(QStringLiteral("stop")); }
    void Play() { send(QStringLiteral("play")); }
    void Seek(qlonglong offsetUs) {
        if (m_player->canSeek())
            send(QStringLiteral("seek"), qMax<qlonglong>(0, m_player->position() + offsetUs) / 1000);
    }
    void SetPosition(const QDBusObjectPath &track, qlonglong positionUs) {
        if (m_player->canSeek() && track.path() == m_player->trackId() && positionUs >= 0)
            send(QStringLiteral("seek"), positionUs / 1000);
    }
    void OpenUri(const QString &) {}

  Q_SIGNALS:
    void Seeked(qlonglong position);

  private:
    void send(const QString &command, qint64 positionMs = 0) {
        Q_EMIT m_player->command(command, positionMs);
    }

    MprisPlayer *m_player;
};

MprisPlayer::MprisPlayer(const Validate::Media &first, const QString &artDir, QObject *parent)
    : QObject(parent)
    , m_bus(QDBusConnection::connectToBus(QDBusConnection::SessionBus,
                                          QStringLiteral("deckard-mpris-%1").arg(++gConnections)))
    , m_artDir(artDir)
    , m_state(first) {
    m_since.start();
    m_busName = QStringLiteral("org.mpris.MediaPlayer2.waydroid_%1.instance%2")
                    .arg(QString(first.pkg).replace(QLatin1Char('.'), QLatin1Char('_')))
                    .arg(first.sid);
    if (first.hasArt)
        setArt(first.art);
    new MprisRootAdaptor(this);
    new MprisPlayerAdaptor(this);
    m_registered = m_bus.isConnected() && m_bus.registerObject(kPath, this) &&
                   m_bus.registerService(m_busName);
    if (!m_registered)
        qCWarning(lcMpris) << "cannot register" << m_busName << m_bus.lastError().message();
}

MprisPlayer::~MprisPlayer() {
    if (!m_artPath.isEmpty())
        QFile::remove(m_artPath);
    m_bus.unregisterService(m_busName);
    QDBusConnection::disconnectFromBus(m_bus.name());
}

bool MprisPlayer::isRegistered() const {
    return m_registered;
}

QString MprisPlayer::trackId() const {
    return QStringLiteral("/org/marathonos/deckard/session%1/track%2").arg(m_state.sid).arg(m_track);
}

QVariantMap MprisPlayer::metadata() const {
    QVariantMap md{{QStringLiteral("mpris:trackid"), QVariant::fromValue(QDBusObjectPath(trackId()))},
                   {QStringLiteral("xesam:title"), m_state.title}};
    if (m_state.durationMs > 0)
        md.insert(QStringLiteral("mpris:length"), qlonglong(m_state.durationMs * 1000));
    if (!m_state.artist.isEmpty())
        md.insert(QStringLiteral("xesam:artist"), QStringList{m_state.artist});
    if (!m_state.album.isEmpty())
        md.insert(QStringLiteral("xesam:album"), m_state.album);
    if (!m_artPath.isEmpty())
        md.insert(QStringLiteral("mpris:artUrl"), QStringLiteral("file://") + m_artPath);
    return md;
}

qlonglong MprisPlayer::position() const {
    qint64 ms = m_state.positionMs;
    if (m_state.status == QLatin1String("Playing"))
        ms += qint64(m_since.elapsed() * m_state.speed);
    if (m_state.durationMs > 0)
        ms = qMin(ms, m_state.durationMs);
    return qMax<qint64>(0, ms) * 1000;
}

void MprisPlayer::setArt(const QByteArray &png) {
    if (!m_artPath.isEmpty())
        QFile::remove(m_artPath);
    m_artPath.clear();
    const QImage image = Images::decode(png, 512);
    if (image.isNull())
        return;
    static int serial = 0;
    const QString path = QStringLiteral("%1/%2-%3.png").arg(m_artDir).arg(m_state.sid).arg(++serial);
    if (Images::save(image, path))
        m_artPath = path;
}

void MprisPlayer::update(const Validate::Media &s) {
    const qlonglong expected = position();
    QVariantMap     changed;
    const bool      newTrack = s.title != m_state.title || s.artist != m_state.artist ||
                          s.album != m_state.album || s.durationMs != m_state.durationMs;
    if (newTrack)
        ++m_track;
    const QString oldArt = m_artPath;
    if (s.hasArt)
        setArt(s.art);
    const Validate::Media old = m_state;
    m_state                   = s;
    m_since.restart();
    if (newTrack || m_artPath != oldArt)
        changed.insert(QStringLiteral("Metadata"), metadata());
    if (s.status != old.status)
        changed.insert(QStringLiteral("PlaybackStatus"), s.status);
    if (s.speed != old.speed)
        changed.insert(QStringLiteral("Rate"), rate());
    if (s.canPlay != old.canPlay)
        changed.insert(QStringLiteral("CanPlay"), s.canPlay);
    if (s.canPause != old.canPause)
        changed.insert(QStringLiteral("CanPause"), s.canPause);
    if (s.canNext != old.canNext)
        changed.insert(QStringLiteral("CanGoNext"), s.canNext);
    if (s.canPrevious != old.canPrevious)
        changed.insert(QStringLiteral("CanGoPrevious"), s.canPrevious);
    changed.insert(QStringLiteral("CanSeek"), canSeek());
    propertiesChanged(kPlayer, changed);
    if (!newTrack && qAbs(position() - expected) > 1500000)
        Q_EMIT seeked(position());
}

void MprisPlayer::propertiesChanged(const QString &interface, const QVariantMap &changed) {
    QDBusMessage signal = QDBusMessage::createSignal(
        kPath, QStringLiteral("org.freedesktop.DBus.Properties"), QStringLiteral("PropertiesChanged"));
    signal << interface << changed << QStringList();
    m_bus.send(signal);
}

Mpris::Mpris(Bridge *bridge, const QString &artDir, QObject *parent)
    : QObject(parent)
    , m_bridge(bridge)
    , m_artDir(artDir) {
    QDir(artDir).removeRecursively();
    QDir().mkpath(artDir);
}

void Mpris::handle(const QString &type, const QJsonObject &message) {
    if (type == QLatin1String("media_gone")) {
        m_players.erase(message.value(QStringLiteral("sid")).toInt());
        return;
    }
    if (type != QLatin1String("media"))
        return;
    QString                              error;
    const std::optional<Validate::Media> state = Validate::media(message, &error);
    if (!state) {
        qCWarning(lcMpris) << "rejected a media session:" << error;
        return;
    }
    const auto it = m_players.find(state->sid);
    if (it != m_players.end()) {
        it->second->update(*state);
        return;
    }
    if (m_players.size() >= size_t(kMaxPlayers)) {
        qCWarning(lcMpris) << "too many media sessions; ignored one from" << state->pkg;
        return;
    }
    auto player = std::make_unique<MprisPlayer>(*state, m_artDir);
    if (!player->isRegistered())
        return;
    const int sid = state->sid;
    connect(player.get(), &MprisPlayer::command, this,
            [this, sid](const QString &command, qint64 positionMs) {
                m_bridge->send({{QStringLiteral("t"), QStringLiteral("media_cmd")},
                                {QStringLiteral("sid"), sid},
                                {QStringLiteral("cmd"), command},
                                {QStringLiteral("position"), double(positionMs)}});
            });
    qCInfo(lcMpris) << "player for" << state->pkg << "session" << sid;
    m_players.emplace(sid, std::move(player));
}

void Mpris::clear() {
    m_players.clear();
}

#include "mpris.moc"
