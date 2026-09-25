"""Software-only lifecycle tests: python3 -m unittest discover -s tests."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


HANDLER = Path(__file__).resolve().parents[1] / "contrib/handler-scan-to-pdf.sh"


class PdfHandlerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.output = self.root / "scans"
        self.env = dict(os.environ, PATH=f"{self.bin}:/usr/bin:/bin",
                        SCAN_DIR=str(self.output), SCAN_DEVICE="exact device:123",
                        TRACE=str(self.root / "trace"))
        self.env.pop("SANE_CONFIG_DIR", None)
        self.env["TMPDIR"] = str(self.root)
        self.stub("logger", 'printf "%s\\n" "$*" >> "$TRACE"')
        self.stub("date", 'echo 20260924-120000')
        self.stub("scanimage", '''
printf '%s\\n' "$@" >> "$TRACE"
if [ "${1:-}" = -L ]; then
    printf 'discovery_config=%s\\n' "${SANE_CONFIG_DIR-unset}" >> "$TRACE"
    if [ -f "${SANE_CONFIG_DIR:-}/dll.conf" ]; then
        [ "$(< "$SANE_CONFIG_DIR/dll.conf")" = fujitsu ] || exit 80
        [ -d "$SANE_CONFIG_DIR/dll.d" ] || exit 81
        [ "$(< "$SANE_CONFIG_DIR/fujitsu.conf")" = 'usb 0x04c5 0x11a2' ] || exit 82
        [ "${FAST_FAIL:-}" = yes ] && exit 9
        if [ "${FAST_EMPTY:-}" = yes ]; then
            printf '\\nNo scanners were identified. If you were expecting something different,\\ncheck that the scanner is plugged in, turned on and detected.\\n'
            exit 0
        fi
    fi
    printf '%s\\n' "${DEVICE_LIST:-}"
    exit "${DISCOVERY_STATUS:-0}"
fi
printf 'acquisition_config=%s\\n' "${SANE_CONFIG_DIR-unset}" >> "$TRACE"
for arg in "$@"; do
    case "$arg" in --batch=*) batch=${arg#--batch=};; esac
done
if [ "${CASE:-}" = empty ]; then
    printf 'Document feeder out of documents / Batch terminated, 0 pages scanned\\n' >&2
    exit 7
fi
[ "${CASE:-}" = empty_good ] && exit 0
if [ "${CASE:-}" = other_file ]; then
    printf 'diagnostic' > "${batch%/*}/diagnostic.txt"
    exit 7
fi
printf 'page one' > "${batch//%04d/0001}"
printf 'scanner diagnostic\\n' >&2
[ "${CASE:-}" = partial ] && exit 7
if [ "${CASE:-}" = signal ]; then kill -TERM "$PPID"; exit 1; fi
printf 'page two' > "${batch//%04d/0002}"
exit 0
''')
        self.stub("img2pdf", '''
printf 'converter\\n' >> "$TRACE"
while [ "$#" -gt 0 ]; do
    if [ "$1" = -o ]; then shift; output=$1; fi
    shift
done
[ "${CASE:-}" = blank ] && { : > "$output"; exit 0; }
printf 'complete PDF' > "$output"
[ "${CASE:-}" = conversion ] && exit 42
exit 0
''')

    def stub(self, name, body):
        path = self.bin / name
        path.write_text("#!/bin/bash\n" + body + "\n")
        path.chmod(0o755)

    def run_handler(self, case="success", profile="standard"):
        return subprocess.run(["/bin/bash", str(HANDLER), "scan", profile],
                              env=dict(self.env, CASE=case), text=True,
                              capture_output=True, timeout=10, cwd=self.root)

    def assert_failure(self, result, pages=False):
        self.assertNotEqual(result.returncode, 0, result)
        trace = (self.root / "trace").read_text() if (self.root / "trace").exists() else ""
        self.assertNotIn("Saved", trace + result.stderr)
        self.assertFalse(list(self.output.glob("*.pdf")))
        if pages:
            recovered = list(self.output.glob(".s1500d-*/page_*.tiff"))
            self.assertTrue(recovered, result.stderr)
            self.assertIn(str(recovered[0].parent), result.stderr)
            self.assertEqual(recovered[0].parent.stat().st_mode & 0o777, 0o700)

    def test_success(self):
        result = self.run_handler()
        self.assertEqual(result.returncode, 0, result.stderr)
        pdf, = self.output.glob("*.pdf")
        self.assertEqual(pdf.read_text(), "complete PDF")
        self.assertEqual(pdf.stat().st_mode & 0o777, 0o640)
        self.assertFalse(list(self.output.glob(".s1500d-*")))
        self.assertIn("--device-name=exact device:123", (self.root / "trace").read_text())
        self.assertIn("scanner diagnostic", result.stderr)

    def test_partial_acquisition(self):
        self.assert_failure(self.run_handler("partial"), pages=True)
        self.assertNotIn("converter", (self.root / "trace").read_text())

    def test_conversion_failure(self):
        self.assert_failure(self.run_handler("conversion"), pages=True)

    def test_empty_converter_output(self):
        self.assert_failure(self.run_handler("blank"), pages=True)

    def test_empty_feeder(self):
        for case in ("empty", "empty_good", "empty"):
            with self.subTest(case=case):
                result = self.run_handler(case)
                self.assert_failure(result)
                self.assertIn("No pages scanned", result.stderr)
                self.assertNotIn("Recovery files retained", result.stderr)
                self.assertFalse(list(self.output.glob(".s1500d-*")))

    def test_no_pages_does_not_remove_other_files(self):
        result = self.run_handler("other_file")
        self.assert_failure(result)
        diagnostic, = self.output.glob(".s1500d-*/diagnostic.txt")
        self.assertEqual(diagnostic.read_text(), "diagnostic")
        self.assertIn(str(diagnostic.parent), result.stderr)

    def test_relative_directory_with_cdpath(self):
        self.env.update(SCAN_DIR="scans", CDPATH=str(self.root))
        result = self.run_handler()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(list(self.output.glob("*.pdf"))), 1)

    def test_signal_preserves_pages(self):
        self.assert_failure(self.run_handler("signal"), pages=True)

    def test_missing_dependency(self):
        (self.bin / "img2pdf").unlink()
        for name in ("mkdir", "mktemp", "rm", "chmod", "ln"):
            target = Path("/usr/bin") / name
            (self.bin / name).symlink_to(target)
        self.env["PATH"] = str(self.bin)
        self.assert_failure(self.run_handler())
        self.assertFalse(list(self.output.glob(".s1500d-*/page_*.tiff")))

    def test_destination_failure(self):
        self.output.write_text("not a directory")
        self.assert_failure(self.run_handler())

    @unittest.skipIf(os.geteuid() == 0, "root bypasses directory write permissions")
    def test_unwritable_destination(self):
        self.output.mkdir(mode=0o500)
        try:
            self.assert_failure(self.run_handler())
        finally:
            self.output.chmod(0o700)

    def test_failed_publication(self):
        self.stub("ln", "exit 1")
        result = self.run_handler()
        self.assert_failure(result, pages=True)
        self.assertIn("hard-link support", result.stderr)

    def test_logger_failure_does_not_lose_success(self):
        self.stub("logger", "exit 1")
        result = self.run_handler()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Saved", result.stderr)

    def test_collision(self):
        self.assertEqual(self.run_handler().returncode, 0)
        pdf, = self.output.glob("*.pdf")
        pdf.write_text("original")
        result = self.run_handler()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(pdf.read_text(), "original")
        self.assertIn("Recovery", result.stderr)
        self.assertIn("Destination already exists", result.stderr)
        self.assertTrue(list(self.output.glob(".s1500d-*/page_*.tiff")))

    def test_concurrent_collision(self):
        args = ["/bin/bash", str(HANDLER), "scan", "standard"]
        processes = [subprocess.Popen(args, env=self.env, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, text=True) for _ in range(2)]
        for process in processes:
            process.communicate(timeout=10)
        self.assertEqual(sorted(p.returncode for p in processes), [0, 1])
        pdf, = self.output.glob("*.pdf")
        self.assertEqual(pdf.read_text(), "complete PDF")
        self.assertEqual(len(list(self.output.glob(".s1500d-*/page_*.tiff"))), 2)

    def discover(self, listing, status=0, profile="standard"):
        self.env.pop("SCAN_DEVICE", None)
        self.env.update(DEVICE_LIST=listing, DISCOVERY_STATUS=str(status))
        return self.run_handler(profile=profile)

    def test_detect_single_s1500(self):
        listing = "device `fujitsu:ScanSnap S1500:000000' is a FUJITSU ScanSnap S1500 scanner"
        result = self.discover(listing)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Auto-detected scanner: fujitsu:ScanSnap S1500:000000", result.stderr)
        self.assertIn("--device-name=fujitsu:ScanSnap S1500:000000", (self.root / "trace").read_text())

    def test_no_matching_scanner(self):
        listing = "device `fujitsu:ScanSnap S1500M:123' is a FUJITSU ScanSnap S1500M scanner"
        result = self.discover(listing)
        self.assert_failure(result)
        self.assertIn(listing, result.stderr)
        self.assertIn("SCAN_DEVICE", result.stderr)
        self.assertFalse(self.output.exists())

    def test_multiple_scanners(self):
        listing = "\n".join(f"device `fujitsu:ScanSnap S1500:{serial}' is a FUJITSU ScanSnap S1500 scanner" for serial in (123, 456))
        result = self.discover(listing)
        self.assert_failure(result)
        self.assertIn(listing, result.stderr)
        self.assertIn("SCAN_DEVICE", result.stderr)

    def test_failed_discovery_does_not_use_partial_list(self):
        listing = "device `fujitsu:ScanSnap S1500:123' is a FUJITSU ScanSnap S1500 scanner"
        result = self.discover(listing, status=9)
        self.assert_failure(result)
        self.assertIn("exit 9", result.stderr)
        self.assertIn(listing, result.stderr)
        self.assertNotIn("--batch=", (self.root / "trace").read_text())

    def test_explicit_device_skips_discovery(self):
        self.env["DISCOVERY_STATUS"] = "9"
        result = self.run_handler()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("-L", (self.root / "trace").read_text().splitlines())

    def test_explicit_empty_device_fails_without_lookup(self):
        self.env["SCAN_DEVICE"] = ""
        result = self.run_handler()
        self.assert_failure(result)
        self.assertIn("SCAN_DEVICE is empty", result.stderr)
        self.assertFalse((self.root / "trace").exists() and "-L" in (self.root / "trace").read_text().splitlines())

    def test_other_devices_do_not_make_single_s1500_ambiguous(self):
        listing = "device `other:123' is another scanner\ndevice `fujitsu:ScanSnap S1500:456' is a FUJITSU ScanSnap S1500 scanner"
        result = self.discover(listing)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--device-name=fujitsu:ScanSnap S1500:456", (self.root / "trace").read_text())

    def test_empty_discovery_list(self):
        result = self.discover("")
        self.assert_failure(result)
        self.assertIn("(no devices listed)", result.stderr)

    def fast_config(self):
        (self.root / "fujitsu.conf").write_text("usb 0x04c5 0x11a2\n")
        return "device `fujitsu:ScanSnap S1500:123' is a FUJITSU ScanSnap S1500 scanner"

    def test_fujitsu_only_discovery_is_private_and_temporary(self):
        result = self.discover(self.fast_config())
        self.assertEqual(result.returncode, 0, result.stderr)
        trace = (self.root / "trace").read_text()
        self.assertIn("discovery_config=" + str(self.root / "s1500d-sane-"), trace)
        self.assertIn("acquisition_config=unset", trace)
        self.assertFalse(list(self.root.glob("s1500d-sane-*")))

    def test_explicit_sane_config_semantics_are_unchanged(self):
        listing = self.fast_config()
        for index, value in enumerate(("", str(self.root / "custom"), str(self.root / "one") + ":" + str(self.root / "two") + ":")):
            with self.subTest(value=value):
                self.env["SANE_CONFIG_DIR"] = value
                self.env.pop("SCAN_DEVICE", None)
                result = self.discover(listing, profile=f"scan{index}")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("discovery_config=" + value + "\n", (self.root / "trace").read_text())
                self.assertIn("acquisition_config=" + value + "\n", (self.root / "trace").read_text())
                self.assertNotIn("Fujitsu-only", result.stderr)
        self.assertFalse(list(self.root.glob("s1500d-sane-*")))

    def test_failed_fast_lookup_falls_back(self):
        self.env["FAST_FAIL"] = "yes"
        result = self.discover(self.fast_config())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "trace").read_text().splitlines().count("-L"), 2)
        self.assertFalse(list(self.root.glob("s1500d-sane-*")))

    def test_empty_fast_lookup_falls_back(self):
        self.env["FAST_EMPTY"] = "yes"
        result = self.discover(self.fast_config())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "trace").read_text().splitlines().count("-L"), 2)

    def test_failed_config_copy_falls_back(self):
        self.stub("cp", "exit 1")
        result = self.discover(self.fast_config())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("discovery_config=unset", (self.root / "trace").read_text())
        self.assertFalse(list(self.root.glob("s1500d-sane-*")))

    def test_invalid_profile(self):
        self.assert_failure(self.run_handler(profile="../escape"))


if __name__ == "__main__":
    unittest.main()
