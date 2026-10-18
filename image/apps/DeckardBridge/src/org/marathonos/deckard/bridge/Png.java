package org.marathonos.deckard.bridge;

import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.drawable.BitmapDrawable;
import android.graphics.drawable.Drawable;
import android.util.Base64;

import java.io.ByteArrayOutputStream;

final class Png {
    private Png() {}

    static String encode(Drawable drawable, int max) {
        if (drawable == null)
            return null;
        if (drawable instanceof BitmapDrawable && ((BitmapDrawable) drawable).getBitmap() != null)
            return encode(((BitmapDrawable) drawable).getBitmap(), max);
        int width = drawable.getIntrinsicWidth() > 0 ? drawable.getIntrinsicWidth() : max;
        int height = drawable.getIntrinsicHeight() > 0 ? drawable.getIntrinsicHeight() : max;
        float scale = Math.min(1f, (float) max / Math.max(width, height));
        width = Math.max(1, Math.round(width * scale));
        height = Math.max(1, Math.round(height * scale));
        Bitmap bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888);
        Canvas canvas = new Canvas(bitmap);
        drawable.setBounds(0, 0, width, height);
        drawable.draw(canvas);
        return encode(bitmap, max);
    }

    static String encode(Bitmap bitmap, int max) {
        if (bitmap == null || bitmap.getWidth() <= 0 || bitmap.getHeight() <= 0)
            return null;
        if (bitmap.getConfig() == Bitmap.Config.HARDWARE)
            bitmap = bitmap.copy(Bitmap.Config.ARGB_8888, false);
        int longest = Math.max(bitmap.getWidth(), bitmap.getHeight());
        if (longest > max) {
            float scale = (float) max / longest;
            bitmap = Bitmap.createScaledBitmap(bitmap, Math.max(1, Math.round(bitmap.getWidth() * scale)),
                    Math.max(1, Math.round(bitmap.getHeight() * scale)), true);
        }
        ByteArrayOutputStream png = new ByteArrayOutputStream();
        bitmap.compress(Bitmap.CompressFormat.PNG, 100, png);
        return Base64.encodeToString(png.toByteArray(), Base64.NO_WRAP);
    }
}
