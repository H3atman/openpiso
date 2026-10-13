-- Client Guard Coverage diagnostic (ADR-0076 / #344): the last Guard
-- coverage outcome a Client Heartbeat reported for this PC. NULL until a
-- Client that reports the field is seen, so an older Client reads as
-- unsupported or unknown and can never create a false Guard Alert. Strictly
-- diagnostic: it never changes identity, session, money, time, or Points.
ALTER TABLE pcs ADD COLUMN last_guard_coverage TEXT;
