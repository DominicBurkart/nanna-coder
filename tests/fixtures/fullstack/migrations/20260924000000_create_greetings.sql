CREATE TABLE IF NOT EXISTS greetings (
    id BIGSERIAL PRIMARY KEY,
    message TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO greetings (message) VALUES ('Hello from the full-stack fixture');
