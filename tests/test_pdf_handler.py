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
        self.stub("logger", 'printf "%s\\n" "$*" >> "$TRACE"')
        self.stub("date", 'echo 20260924-120000')
        self.stub("scanimage", '''
printf '%s\\n' "$@" >> "$TRACE"
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

    def test_missing_device(self):
        self.env.pop("SCAN_DEVICE")
        self.assert_failure(self.run_handler())

    def test_invalid_profile(self):
        self.assert_failure(self.run_handler(profile="../escape"))


if __name__ == "__main__":
    unittest.main()
