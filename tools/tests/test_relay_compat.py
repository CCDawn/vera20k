"""Which released clients the relay compatibility check plays against."""
import unittest

from tools.relay_compat import supported_clients


def release(tag, created, prerelease=False, draft=False):
    return {"tagName": tag, "createdAt": created, "isPrerelease": prerelease, "isDraft": draft}


class SupportedClientsTest(unittest.TestCase):
    def test_latest_stable_and_newest_dev_build(self):
        rows = [
            release("v0.2.2", "2026-10-27T09:17:12Z"),
            release("v0.2.3", "2026-10-29T11:13:36Z"),
            release("v0.2.4", "2026-10-30T09:00:00Z", draft=True),
            release("v0.2.3-20261028-162610", "2026-10-28T16:52:56Z", prerelease=True),
            release("v0.2.3-20261029-101544", "2026-10-29T10:34:35Z", prerelease=True),
            release("v0.2.4-rc1", "2026-10-30T10:00:00Z", prerelease=True),
        ]
        self.assertEqual(supported_clients(rows), ["v0.2.3", "v0.2.3-20261029-101544"])


if __name__ == "__main__":
    unittest.main()
