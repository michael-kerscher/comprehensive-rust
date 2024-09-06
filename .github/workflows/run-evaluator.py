import subprocess
import sys
import logging
from pathlib import Path

from selenium import webdriver
from selenium.webdriver.chrome.service import Service
from selenium.webdriver.chrome.options import Options
from webdriver_manager.chrome import ChromeDriverManager

logger = logging.getLogger(__name__)
logging.basicConfig(encoding="utf-8", level=logging.DEBUG)

PORT = 4444
BOOK_DIRECTORY = Path(sys.argv[1])
VIOLATIONS_FILE = Path(sys.argv[2])
EXTRA_OPTIONS = sys.argv[3:]

logger.debug(f"using extra options: {EXTRA_OPTIONS}")
options = Options()
options.add_argument("--headless")
options.add_argument("--window-size=1920,1080")

# install chrome driver
chrome_driver = ChromeDriverManager().install()
# run the chrome driver on the configured port
driver = webdriver.Chrome(service=Service(chrome_driver, port=PORT),
                          options=options)

SUBPROCESS = [
    "mdbook-slide-evaluator",
    "--webdriver",
    f"http://localhost:{PORT}",
    "--export",
    # provide absolute path
    VIOLATIONS_FILE.resolve(),
]
# extra options are redirected to the evaluator
SUBPROCESS.extend(EXTRA_OPTIONS)
# the last argument is the base directory containing the html files
SUBPROCESS.append(BOOK_DIRECTORY.resolve())

# run the mdbook-evaluator
subprocess.run(SUBPROCESS)
