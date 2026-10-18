#pragma once

#include <QJsonObject>
#include <QObject>

class Bridge;

// Keeps Android as quiet as Marathon: Marathon's silent and vibrate profiles
// and Do Not Disturb put Android in alarms-only Do Not Disturb, and Android's
// alarm stream follows Marathon's alarm volume.
class AudioSync : public QObject {
    Q_OBJECT

  public:
    explicit AudioSync(Bridge *bridge, QObject *parent = nullptr);

    void resend();

  private Q_SLOTS:
    void refresh();
    void onSettingChanged(const QString &name);

  private:
    Bridge     *m_bridge;
    QJsonObject m_last;
};
