// Download the official Kasane Teto UTAU voicebank using Playwright.
// Usage: npm i playwright && node scripts/fetch-teto.js [destination-dir]
//
// No audio is bundled in TeToS. You supply the voicebank; you obey its license.

const { chromium } = require('playwright');
const path = require('path');

const ARCHIVE = 'TETO-tougou-110401.zip'; // standard CV + VCV, the good one
const URL = `https://kasaneteto.jp/assets/download/utau/${ARCHIVE}`;
const destDir = process.argv[2] || path.resolve(__dirname, '../../UtauTTS/voice');

(async () => {
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ acceptDownloads: true });
  const page = await context.newPage();

  await page.goto('https://kasaneteto.jp/utau/', { waitUntil: 'domcontentloaded', timeout: 60_000 });
  const link = page.locator(`a[href$="${ARCHIVE}"]`).first();
  if (await link.count() === 0) throw new Error('download link not found on official page');

  const [download] = await Promise.all([
    page.waitForEvent('download', { timeout: 180_000 }),
    link.click(),
  ]);
  const dest = path.join(destDir, ARCHIVE);
  await download.saveAs(dest);
  console.log(`saved ${dest}`);
  console.log('next: unzip it, then point TeToS --voice-dir at the extracted 重音テト音声ライブラリー folder');
  await browser.close();
})().catch((err) => { console.error(err); process.exit(1); });
