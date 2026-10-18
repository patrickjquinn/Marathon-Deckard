package org.marathonos.deckard.ime;

import android.net.LocalSocket;
import android.net.LocalSocketAddress;
import android.os.Handler;
import android.util.Log;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.BufferedInputStream;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/** Length-prefixed JSON to deckardd's IME channel (protocol/bridge.md). */
final class Bridge implements Runnable {
    interface Listener {
        void onConnected();

        void onMessage(JSONObject message);
    }

    static final String TAG = "DeckardIme";
    private static final String PATH = "/data/deckard/ime.sock";
    private static final int MAX_FRAME = 64 * 1024;

    private final Listener listener;
    private final Handler main;
    private final ExecutorService writer = Executors.newSingleThreadExecutor();
    private volatile boolean running;
    private Thread thread;
    private LocalSocket socket;
    private DataOutputStream out;

    Bridge(Listener listener, Handler main) {
        this.listener = listener;
        this.main = main;
    }

    void start() {
        running = true;
        thread = new Thread(this, "deckard-ime-bridge");
        thread.start();
    }

    void stop() {
        running = false;
        closeSocket();
        writer.shutdownNow();
    }

    void send(JSONObject message) {
        final byte[] frame = message.toString().getBytes(StandardCharsets.UTF_8);
        if (frame.length > MAX_FRAME)
            return;
        writer.execute(() -> {
            synchronized (this) {
                if (out == null)
                    return;
                try {
                    out.writeInt(frame.length);
                    out.write(frame);
                    out.flush();
                } catch (IOException e) {
                    closeSocket();
                }
            }
        });
    }

    @Override
    public void run() {
        while (running) {
            final LocalSocket s = new LocalSocket();
            try {
                s.connect(new LocalSocketAddress(PATH, LocalSocketAddress.Namespace.FILESYSTEM));
                synchronized (this) {
                    socket = s;
                    out = new DataOutputStream(s.getOutputStream());
                }
                main.post(listener::onConnected);
                final DataInputStream in = new DataInputStream(
                        new BufferedInputStream(s.getInputStream()));
                while (running) {
                    final int length = in.readInt();
                    if (length <= 0 || length > MAX_FRAME)
                        throw new IOException("bad frame length " + length);
                    final byte[] frame = new byte[length];
                    in.readFully(frame);
                    try {
                        final JSONObject message =
                                new JSONObject(new String(frame, StandardCharsets.UTF_8));
                        main.post(() -> listener.onMessage(message));
                    } catch (JSONException e) {
                        Log.w(TAG, "bad message from host");
                    }
                }
            } catch (IOException e) {
                // deckardd not up yet, or it went away
            } finally {
                closeSocket();
                try {
                    s.close();
                } catch (IOException ignored) {
                }
            }
            if (!running)
                break;
            try {
                Thread.sleep(1000);
            } catch (InterruptedException e) {
                break;
            }
        }
    }

    private synchronized void closeSocket() {
        out = null;
        if (socket != null) {
            try {
                socket.shutdownInput();
            } catch (IOException ignored) {
            }
            try {
                socket.close();
            } catch (IOException ignored) {
            }
            socket = null;
        }
    }
}
