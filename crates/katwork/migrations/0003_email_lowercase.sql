UPDATE users SET email = lower(btrim(email)) WHERE email <> lower(btrim(email));
ALTER TABLE users ADD CONSTRAINT users_email_lowercase CHECK (email = lower(email));
