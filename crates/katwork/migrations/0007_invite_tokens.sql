-- Invite and reset tokens share the token mechanics but never each other's routes.
ALTER TABLE password_resets ADD COLUMN kind TEXT NOT NULL DEFAULT 'reset' CHECK (kind IN ('reset', 'invite'));
