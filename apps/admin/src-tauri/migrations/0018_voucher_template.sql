-- Shop-wide Voucher Template used only by the owner's preview and print iframe.
-- Page layout intentionally lives in this CSS rather than application chrome.
INSERT INTO settings (key, value, updated_at)
VALUES
  ('voucher_template_html', '<article class="voucher-card">
  <header><strong>{{shop_name}}</strong><span>{{prefix}}</span></header>
  <div class="voucher-code">{{code}}</div>
  <dl>
    <div><dt>Time</dt><dd>{{time}}</dd></div>
    <div><dt>Points</dt><dd>{{points}}</dd></div>
    <div><dt>Price</dt><dd>{{price}}</dd></div>
    <div><dt>Expires</dt><dd>{{expires_at}}</dd></div>
  </dl>
</article>', CURRENT_TIMESTAMP),
  ('voucher_template_css', '@page { size: A4 portrait; margin: 10mm; }
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
@media print { .voucher-card { break-inside: avoid; page-break-inside: avoid; } }', CURRENT_TIMESTAMP)
ON CONFLICT(key) DO NOTHING;
