import { readdirSync, readFileSync } from "node:fs";
import { basename, relative, resolve } from "node:path";

type Violation = {
  file: string;
  line: number;
  text: string;
};

const clientDir = resolve(import.meta.dir, "..");
const widgetFile = resolve(clientDir, "src/views/Widget.svelte");
const componentsDir = resolve(clientDir, "src/lib/components");
const violations: Violation[] = [];

function checkFile(filePath: string): void {
  const relativePath = relative(clientDir, filePath).replaceAll("\\", "/");
  const lines = readFileSync(filePath, "utf8").split("\n");

  for (const [index, line] of lines.entries()) {
    if (/\bfixed\b/.test(line)) {
      violations.push({
        file: relativePath,
        line: index + 1,
        text: line.trim(),
      });
    }
  }
}

checkFile(widgetFile);

for (const filePath of readdirSync(componentsDir, {
  recursive: true,
  withFileTypes: true,
})) {
  if (
    filePath.isFile() &&
    filePath.name.endsWith(".svelte") &&
    basename(filePath.name) !== "WidgetOverlay.svelte"
  ) {
    checkFile(resolve(filePath.parentPath, filePath.name));
  }
}

if (violations.length > 0) {
  console.info("ERROR: Raw fixed CSS found outside WidgetOverlay.svelte\n");
  for (const violation of violations) {
    console.info(`  ${violation.file}:${violation.line}  ${violation.text}`);
  }
  console.info(
    "\nAll fixed overlays in the Widget view tree must go through the WidgetOverlay component.",
  );
  process.exit(1);
}

console.info("OK: No raw fixed CSS outside WidgetOverlay.svelte");
