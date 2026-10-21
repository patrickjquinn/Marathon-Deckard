#include "palette.h"

#include <QDBusConnection>
#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QLoggingCategory>
#include <QSaveFile>

#include <array>
#include <cmath>

Q_LOGGING_CATEGORY(lcPalette, "deckard.palette")

namespace {

    const std::array<int, 13> kShades{0, 10, 50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 1000};
    const double              kD65[3] = {0.95047, 1.0, 1.08883};

    // MColors.qml tokens the palette pins (shell/qml/theme/MColors.qml).
    const QColor kElev1(0x0a0a0b), kElev2(0x121213), kElev3(0x1c1c1d), kElev4(0x282829),
        kElev5(0x353536), kBackground(0x040404), kTextOnAccent(QRgb(0xff000000));

    double toLinear(double c) {
        return c <= 0.04045 ? c / 12.92 : std::pow((c + 0.055) / 1.055, 2.4);
    }
    double fromLinear(double c) {
        return c <= 0.0031308 ? 12.92 * c : 1.055 * std::pow(c, 1 / 2.4) - 0.055;
    }
    double fLab(double t) {
        constexpr double d = 6.0 / 29;
        return t > d * d * d ? std::cbrt(t) : t / (3 * d * d) + 4.0 / 29;
    }
    double fLabInv(double t) {
        constexpr double d = 6.0 / 29;
        return t > d ? t * t * t : 3 * d * d * (t - 4.0 / 29);
    }

    struct Lch {
        double l, c, h;
    };

    Lch toLch(const QColor &color) {
        const double r = toLinear(color.redF()), g = toLinear(color.greenF()),
                     b = toLinear(color.blueF());
        const double x  = 0.4124564 * r + 0.3575761 * g + 0.1804375 * b;
        const double y  = 0.2126729 * r + 0.7151522 * g + 0.0721750 * b;
        const double z  = 0.0193339 * r + 0.1191920 * g + 0.9503041 * b;
        const double fx = fLab(x / kD65[0]), fy = fLab(y / kD65[1]), fz = fLab(z / kD65[2]);
        const double a = 500 * (fx - fy), bb = 200 * (fy - fz);
        double       h = std::atan2(bb, a) * 180 / M_PI;
        if (h < 0)
            h += 360;
        return {116 * fy - 16, std::hypot(a, bb), h};
    }

    std::array<double, 3> toRgb(double l, double c, double h) {
        const double a = c * std::cos(h * M_PI / 180), b = c * std::sin(h * M_PI / 180);
        const double fy = (l + 16) / 116;
        const double x  = kD65[0] * fLabInv(fy + a / 500);
        const double y  = kD65[1] * fLabInv(fy);
        const double z  = kD65[2] * fLabInv(fy - b / 200);
        const std::array<double, 3> lin{3.2404542 * x - 1.5371385 * y - 0.4985314 * z,
                                        -0.9692660 * x + 1.8760108 * y + 0.0415560 * z,
                                        0.0556434 * x - 0.2040259 * y + 1.0572252 * z};
        std::array<double, 3> rgb{};
        for (int i = 0; i < 3; ++i)
            rgb[i] = lin[i] > 0 ? fromLinear(lin[i]) : -1.0;
        return rgb;
    }

    bool inGamut(const std::array<double, 3> &rgb) {
        for (double c : rgb) {
            if (c < -1e-4 || c > 1 + 1e-4)
                return false;
        }
        return true;
    }

    // The most chroma up to the asked-for amount that sRGB can show at this
    // lightness, found by bisection as gen-palette.py does.
    QColor tone(double l, double chroma, double hue) {
        double lo = 0, hi = chroma;
        if (inGamut(toRgb(l, hi, hue))) {
            lo = hi;
        } else {
            for (int i = 0; i < 30; ++i) {
                const double mid = (lo + hi) / 2;
                (inGamut(toRgb(l, mid, hue)) ? lo : hi) = mid;
            }
        }
        const std::array<double, 3> rgb = toRgb(l, lo, hue);
        auto channel = [](double c) { return int(std::nearbyint(qBound(0.0, c, 1.0) * 255)); };
        return QColor(channel(rgb[0]), channel(rgb[1]), channel(rgb[2]));
    }

    using Ramp = std::array<QColor, 13>;

    Ramp ramp(double chroma, double hue) {
        Ramp r;
        for (size_t i = 0; i < kShades.size(); ++i)
            r[i] = tone(100 - kShades[i] / 10.0, chroma, hue);
        return r;
    }

    void pin(Ramp &r, int shade, const QColor &color) {
        for (size_t i = 0; i < kShades.size(); ++i) {
            if (kShades[i] == shade)
                r[i] = color;
        }
    }

} // namespace

namespace Palette {

    QList<QPair<QString, QColor>> generate(const QColor &highlight) {
        const QColor h(highlight.rgb());
        const Lch    accent = toLch(h);
        const double grey   = toLch(kElev3).h;

        Ramp accent1 = ramp(accent.c, accent.h), accent2 = ramp(16, accent.h),
             neutral1 = ramp(1.5, grey), neutral2 = ramp(4, accent.h);
        pin(accent1, 100, h);
        pin(accent1, 200, h);
        pin(accent1, 800, kTextOnAccent);
        pin(neutral1, 100, QColor(Qt::white));
        pin(neutral1, 700, kElev5);
        pin(neutral1, 800, kElev3);
        pin(neutral1, 900, kBackground);
        pin(neutral2, 200, QColor(0x9b9b9b));
        pin(neutral2, 700, kElev4);
        pin(neutral2, 800, kElev2);
        pin(neutral2, 900, kElev1);

        const std::array<std::pair<const char *, const Ramp *>, 5> palettes{
            {{"accent1", &accent1},
             {"accent2", &accent2},
             {"accent3", &accent1},
             {"neutral1", &neutral1},
             {"neutral2", &neutral2}}};
        QList<QPair<QString, QColor>> out;
        for (const auto &[name, r] : palettes) {
            for (size_t i = 0; i < kShades.size(); ++i)
                out.append({QStringLiteral("system_%1_%2").arg(QLatin1String(name)).arg(kShades[i]),
                            (*r)[i]});
        }
        return out;
    }

} // namespace Palette

AmbiencePalette::AmbiencePalette(const QString &path, QObject *parent)
    : QObject(parent)
    , m_path(path) {
    QDBusConnection bus = QDBusConnection::sessionBus();
    bus.connect(QStringLiteral("org.marathonos.Shell"), QStringLiteral("/org/marathonos/Shell/Ambience"),
                QStringLiteral("org.marathonos.Shell.Ambience1"), QStringLiteral("AmbienceChanged"),
                this, SLOT(refresh()));
    bus.connect(QStringLiteral("org.freedesktop.DBus"), QStringLiteral("/org/freedesktop/DBus"),
                QStringLiteral("org.freedesktop.DBus"), QStringLiteral("NameOwnerChanged"), this,
                SLOT(onNameOwnerChanged(QString)));
    refresh();
}

void AmbiencePalette::onNameOwnerChanged(const QString &name) {
    if (name == QLatin1String("org.marathonos.Shell"))
        refresh();
}

void AmbiencePalette::refresh() {
    const QDBusMessage call = QDBusMessage::createMethodCall(
        QStringLiteral("org.marathonos.Shell"), QStringLiteral("/org/marathonos/Shell/Ambience"),
        QStringLiteral("org.marathonos.Shell.Ambience1"), QStringLiteral("GetAmbience"));
    auto *watcher = new QDBusPendingCallWatcher(QDBusConnection::sessionBus().asyncCall(call), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, watcher] {
        watcher->deleteLater();
        const QDBusPendingReply<QVariantMap> reply = *watcher;
        if (reply.isError()) {
            qCWarning(lcPalette) << "cannot read the ambience:" << reply.error().message();
            return;
        }
        apply(reply.value());
    });
}

void AmbiencePalette::apply(const QVariantMap &ambience) {
    const QColor highlight(
        ambience.value(QStringLiteral("/desktop/jolla/theme/color/highlight")).toString());
    if (!highlight.isValid())
        return;
    QByteArray content;
    for (const auto &[name, color] : Palette::generate(highlight))
        content += name.toLatin1() + ' ' + color.name(QColor::HexArgb).toLatin1() + '\n';
    QFile current(m_path);
    if (current.open(QIODevice::ReadOnly) && current.readAll() == content)
        return;
    current.close();
    QDir().mkpath(QFileInfo(m_path).absolutePath());
    QSaveFile file(m_path);
    if (!file.open(QIODevice::WriteOnly) || file.write(content) != content.size() || !file.commit()) {
        qCWarning(lcPalette) << "cannot write" << m_path;
        return;
    }
    qCInfo(lcPalette) << "palette for highlight" << highlight.name();
}
