-- Bump untouched old grace defaults for onboarding; preserve owner-customized values.
UPDATE settings SET value = '300', updated_at = CURRENT_TIMESTAMP
WHERE key = 'shutdown_grace_seconds' AND value = '30';

UPDATE settings SET value = '600', updated_at = CURRENT_TIMESTAMP
WHERE key = 'offline_grace_seconds' AND value = '120';
