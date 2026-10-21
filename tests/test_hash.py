"""hash_text: SHA-256 и HMAC-SHA256 с солью (не требует polars)."""
import hashlib
import hmac
import unittest

from helpers import dbx


class HashText(unittest.TestCase):
    def test_bytes_are_hashed_as_is(self):
        self.assertEqual(dbx.hash_text(b"\xff\x00"), hashlib.sha256(b"\xff\x00").hexdigest())

    def test_plain_sha256(self):
        self.assertEqual(
            dbx.hash_text("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        )

    def test_none_stays_none(self):
        self.assertIsNone(dbx.hash_text(None))
        self.assertIsNone(dbx.hash_text(None, "salt"))

    def test_salt_gives_hmac(self):
        self.assertEqual(
            dbx.hash_text("The quick brown fox jumps over the lazy dog", "key"),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8",
        )
        self.assertEqual(
            dbx.hash_text("x", "k"),
            hmac.new(b"k", b"x", hashlib.sha256).hexdigest(),
        )

    def test_empty_salt_means_no_salt(self):
        self.assertEqual(dbx.hash_text("abc", ""), dbx.hash_text("abc"))

    def test_non_ascii(self):
        self.assertEqual(dbx.hash_text("привет"), hashlib.sha256("привет".encode("utf-8")).hexdigest())


if __name__ == "__main__":
    unittest.main()
