#pragma once

#include <QElapsedTimer>
#include <QFileSystemWatcher>
#include <QJsonObject>
#include <QList>
#include <QObject>
#include <QSet>
#include <QString>
#include <QTimer>

#include <functional>

// The user's folders that are also Android's shared storage (the LXC hook
// deckard-shared-folders binds them over /data/media/0/...). Android's
// MediaProvider, in group media_rw, gets read and write through ACLs, and
// the user keeps access to what it creates; changes made on the host are
// handed to MediaProvider to index (media_scan to DeckardSystem).
class SharedStorage : public QObject {
    Q_OBJECT

  public:
    struct Folder {
        QString host;    // absolute path
        QString android; // "Pictures", ...
    };
    using Send = std::function<bool(const QJsonObject &)>;

    static constexpr unsigned kMediaRw   = 1023;
    static constexpr int      kSettleMs  = 3000;
    static constexpr int      kMaxPaths  = 64;

    // The XDG user dirs and ~/DCIM, as the hook maps them.
    static QList<Folder> userFolders();

    // stamp: where a finished first pass over the folders is recorded.
    SharedStorage(QList<Folder> folders, QString stamp, Send send, QObject *parent = nullptr);

    // ACLs on the folders, and once (per stamp) on everything in them.
    void prepare();
    // Android came up: index everything once.
    void setConnected(bool connected);

    // The ACL that lets media_rw in (and, as a default, the owner keep
    // access to what others create). False where the filesystem has none.
    static bool grant(const QString &path, bool directory);
    // /storage/emulated/0/... for a host path in a folder; "" outside them.
    QString androidPath(const QString &hostPath) const;

  private:
    void watch();
    void changed(const QString &dir);
    void flush();
    void walk(const QString &dir);

    QList<Folder>      m_folders;
    QString            m_stamp;
    Send               m_send;
    QFileSystemWatcher m_watcher;
    QSet<QString>      m_dirty;
    QTimer             m_settle;
    bool               m_connected = false;
};
