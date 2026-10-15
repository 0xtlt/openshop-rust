-- When the access token of an installation expires. A token obtained with the
-- client credentials grant lasts about 24 hours; the tokens of OAuth and of the
-- token exchange do not expire and leave it empty.
ALTER TABLE installations ADD COLUMN IF NOT EXISTS access_token_expires_at timestamp with time zone;
