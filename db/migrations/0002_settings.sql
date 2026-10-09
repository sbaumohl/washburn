-- Operator-controlled runtime settings (read by the public app, written by the admin app).
CREATE TABLE settings (
    key        text PRIMARY KEY,
    value      jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    updated_by text
);

-- Kill switch for the public site. Turning it off returns 503 on every public route.
INSERT INTO settings (key, value) VALUES ('public_site_enabled', 'true'::jsonb);
