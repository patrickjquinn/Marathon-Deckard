"""deckard-shared-folders against a fake home and Waydroid data, with the
mount and chown recorded instead of done. The container's rootfs is the
data directory's parent, so its "data" is the user's data, as LXC's bind
makes it.
Usage: python3 -I test-deckard-shared-folders.py <path to deckard-shared-folders>
"""
import importlib.machinery
import importlib.util
import os
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
HOOK = sys.argv.pop(1)
loader = importlib.machinery.SourceFileLoader("hook", HOOK)
spec = importlib.util.spec_from_loader("hook", loader)
hook = importlib.util.module_from_spec(spec)
loader.exec_module(hook)


class SharedFolders(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = os.path.join(self.tmp.name, "home")
        self.data = os.path.join(self.home, ".local/share/waydroid/data")
        self.rootfs = os.path.dirname(self.data)
        os.makedirs(os.path.join(self.data, "media/0/Pictures"))
        os.makedirs(os.path.join(self.home, ".config"))
        os.makedirs(os.path.join(self.home, "Bilder"))
        with open(os.path.join(self.home, ".config/user-dirs.dirs"), "w") as f:
            f.write('XDG_PICTURES_DIR="$HOME/Bilder"\nXDG_MUSIC_DIR="$HOME/../etc"\nXDG_VIDEOS_DIR="/etc"\n')
        self.mounts = []
        self.chowns = []

    def tearDown(self):
        self.tmp.cleanup()

    def bind(self, source, target):
        self.mounts.append((os.readlink("/proc/self/fd/%d" % source), os.readlink("/proc/self/fd/%d" % target)))

    def chown(self, target, uid, gid, dir_fd=None, follow_symlinks=True):
        self.chowns.append((target, uid, gid))

    def share(self, data=None):
        return hook.share(self.rootfs, self.home, data or self.data, os.getuid(), os.getgid(),
                          bind=self.bind, chown=self.chown)

    def test_maps_xdg_dirs_and_creates_missing_ones(self):
        self.assertEqual(self.share(), ["Documents", "Download", "Pictures", "Music", "Movies", "DCIM"])
        mounts = dict((os.path.basename(t), os.path.relpath(s, self.home)) for s, t in self.mounts)
        self.assertEqual(mounts, {"Documents": "Documents", "Download": "Downloads", "Pictures": "Bilder",
                                  "Music": "Music", "Movies": "Videos", "DCIM": "DCIM"})
        for _, target in self.mounts:
            self.assertEqual(os.path.dirname(target), os.path.join(self.data, "media/0"))
        for name in ("Documents", "Downloads", "Music", "Videos", "DCIM"):
            self.assertTrue(os.path.isdir(os.path.join(self.home, name)))
        self.assertFalse(os.path.exists(os.path.join(self.tmp.name, "etc")))
        self.assertNotIn("Android", os.listdir(os.path.join(self.data, "media/0")))

    def test_moves_what_android_had(self):
        pictures = os.path.join(self.data, "media/0/Pictures")
        for path, text in (("cat.jpg", "cat"), ("dog.jpg", "android dog")):
            with open(os.path.join(pictures, path), "w") as f:
                f.write(text)
        os.makedirs(os.path.join(pictures, "Screenshots"))
        with open(os.path.join(self.home, "Bilder/dog.jpg"), "w") as f:
            f.write("dog")
        self.share()
        with open(os.path.join(self.home, "Bilder/cat.jpg")) as f:
            self.assertEqual(f.read(), "cat")
        self.assertTrue(os.path.isdir(os.path.join(self.home, "Bilder/Screenshots")))
        with open(os.path.join(self.home, "Bilder/dog.jpg")) as f:
            self.assertEqual(f.read(), "dog")
        self.assertEqual(os.listdir(pictures), ["dog.jpg"])
        self.assertIn(("cat.jpg", os.getuid(), hook.MEDIA_RW), self.chowns)
        mode = os.stat(os.path.join(self.home, "Bilder/cat.jpg")).st_mode
        self.assertEqual(mode & 0o660, 0o660)
        self.assertEqual(mode & 0o002, 0)

    def test_links_are_not_followed(self):
        secret = os.path.join(self.tmp.name, "secret")
        os.makedirs(secret)
        os.symlink(secret, os.path.join(self.home, "Documents"))
        os.rmdir(os.path.join(self.data, "media/0/Pictures"))
        os.symlink(secret, os.path.join(self.data, "media/0/Pictures"))
        shared = self.share()
        self.assertNotIn("Documents", shared)
        self.assertNotIn("Pictures", shared)
        self.assertEqual(os.listdir(secret), [])
        self.assertTrue(all(secret not in s and secret not in t for s, t in self.mounts))

    def test_linked_data_path_refused(self):
        os.rename(os.path.join(self.home, ".local"), os.path.join(self.tmp.name, "elsewhere"))
        os.symlink(os.path.join(self.tmp.name, "elsewhere"), os.path.join(self.home, ".local"))
        self.assertEqual(self.share(), [])
        self.assertEqual(self.mounts, [])

    def test_data_outside_home_refused(self):
        self.assertEqual(self.share("/var/lib/waydroid/data"), [])
        self.assertEqual(self.mounts, [])

    def test_session_data_from_config(self):
        with open(os.path.join(self.tmp.name, "config_session"), "w") as f:
            f.write("lxc.mount.entry = tmpfs run/xdg none create=dir 0 0\n"
                    "lxc.mount.entry = /home/user/.local/share/waydroid/data data none rbind 0 0\n")
        self.assertEqual(hook.session_data(self.tmp.name), "/home/user/.local/share/waydroid/data")


if __name__ == "__main__":
    unittest.main()
