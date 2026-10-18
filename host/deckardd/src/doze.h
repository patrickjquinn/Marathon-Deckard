#pragma once

#include <QObject>
#include <QVariantMap>

// Marathon's Doze as Android's deep idle: writes /data/deckard/doze ("1" or
// "0") when the shell's Power1 state says it entered or left Doze; Android's
// deckard-watch service forces or releases deviceidle.
class DozeSync : public QObject {
    Q_OBJECT

  public:
    DozeSync(const QString &path, QObject *parent = nullptr);

  private Q_SLOTS:
    void onPowerState(const QVariantMap &state);

  private:
    QString m_path;
};
