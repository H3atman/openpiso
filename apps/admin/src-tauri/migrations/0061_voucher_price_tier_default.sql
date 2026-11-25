-- Price-tier default Voucher Template (ADR-0091). Where BOTH the stored HTML and CSS
-- still equal the migration 0018 default exactly, they are replaced with the new
-- default and the default Template Script is set. Customised templates are left
-- alone and get an empty script. Keep the text below byte-identical to
-- DEFAULT_VOUCHER_TEMPLATE_* in src/db/settings.rs.
CREATE TEMP TABLE voucher_default_untouched AS
SELECT 1 AS yes
WHERE (SELECT value FROM settings WHERE key = 'voucher_template_html') = '<article class="voucher-card">
  <header><strong>{{shop_name}}</strong><span>{{prefix}}</span></header>
  <div class="voucher-code">{{code}}</div>
  <dl>
    <div><dt>Time</dt><dd>{{time}}</dd></div>
    <div><dt>Points</dt><dd>{{points}}</dd></div>
    <div><dt>Price</dt><dd>{{price}}</dd></div>
    <div><dt>Expires</dt><dd>{{expires_at}}</dd></div>
  </dl>
</article>'
  AND (SELECT value FROM settings WHERE key = 'voucher_template_css') = '@page { size: A4 portrait; margin: 10mm; }
* { box-sizing: border-box; }
body { margin: 0; color: #111827; font-family: Arial, sans-serif; }
.voucher-grid { display: grid; grid-template-columns: repeat(2, 1fr); gap: 6mm; }
.voucher-card { min-height: 58mm; padding: 6mm; border: 1px dashed #64748b; border-radius: 3mm; break-inside: avoid; page-break-inside: avoid; }
.voucher-card header { display: flex; justify-content: space-between; gap: 4mm; font-size: 10pt; text-transform: uppercase; }
.voucher-code { margin: 8mm 0; font: 700 18pt monospace; text-align: center; letter-spacing: 0.08em; }
.voucher-card dl { display: grid; grid-template-columns: repeat(2, 1fr); gap: 3mm 6mm; margin: 0; }
.voucher-card dl div { display: flex; justify-content: space-between; gap: 2mm; }
.voucher-card dt { color: #64748b; }
.voucher-card dd { margin: 0; font-weight: 700; }
@media print { .voucher-card { break-inside: avoid; page-break-inside: avoid; } }';

INSERT INTO settings (key, value, updated_at)
VALUES ('voucher_template_script',
  CASE WHEN EXISTS (SELECT 1 FROM voucher_default_untouched) THEN '// Tier each card by price: under 50 is low, 50 and over is high, free or unparseable gets none.
for (const card of document.querySelectorAll("[data-price]")) {
  const price = Number(card.dataset.price.replace(/[^\d.]/g, "")); // NaN unless the whole string is a number
  if (price > 0) card.dataset.tier = price < 50 ? "low" : "high";
}' ELSE '' END,
  CURRENT_TIMESTAMP)
ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at
WHERE EXISTS (SELECT 1 FROM voucher_default_untouched);

UPDATE settings SET value = '<article class="voucher-card" data-price="{{price}}">
  <header><strong>{{shop_name}}</strong><span>{{prefix}}</span></header>
  <div class="voucher-code">{{code}}</div>
  <dl>
    <div><dt>Time</dt><dd>{{time}}</dd></div>
    <div><dt>Points</dt><dd>{{points}}</dd></div>
    <div><dt>Price</dt><dd>{{price}}</dd></div>
    <div><dt>Expires</dt><dd>{{expires_at}}</dd></div>
  </dl>
</article>', updated_at = CURRENT_TIMESTAMP
WHERE key = 'voucher_template_html' AND EXISTS (SELECT 1 FROM voucher_default_untouched);

UPDATE settings SET value = '@page { size: A4 portrait; margin: 10mm; }
* { box-sizing: border-box; }
body { margin: 0; color: #111827; font-family: Arial, sans-serif; }
.voucher-grid { display: grid; grid-template-columns: repeat(2, 1fr); gap: 6mm; }
.voucher-card { min-height: 58mm; padding: 6mm; border: 1px dashed #64748b; border-radius: 3mm; break-inside: avoid; page-break-inside: avoid; }
.voucher-card header { display: flex; justify-content: space-between; gap: 4mm; font-size: 10pt; text-transform: uppercase; }
.voucher-code { margin: 8mm 0; font: 700 18pt monospace; text-align: center; letter-spacing: 0.08em; }
.voucher-card dl { display: grid; grid-template-columns: repeat(2, 1fr); gap: 3mm 6mm; margin: 0; }
.voucher-card dl div { display: flex; justify-content: space-between; gap: 2mm; }
.voucher-card[data-tier="low"] .voucher-code { border-top: 4px solid #15803d; }
.voucher-card[data-tier="high"] .voucher-code { border-top: 4px solid #6b1212; }
.voucher-card dt { color: #64748b; }
.voucher-card dd { margin: 0; font-weight: 700; }
@media print { .voucher-card { break-inside: avoid; page-break-inside: avoid; } }', updated_at = CURRENT_TIMESTAMP
WHERE key = 'voucher_template_css' AND EXISTS (SELECT 1 FROM voucher_default_untouched);

DROP TABLE voucher_default_untouched;
