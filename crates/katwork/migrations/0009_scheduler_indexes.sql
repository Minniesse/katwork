CREATE INDEX jobs_thread_id ON jobs (thread_id, id DESC);
CREATE INDEX jobs_requester_created ON jobs (requested_by, created_at, id);
CREATE INDEX tasks_job_kat ON tasks (job_id, kat_id);
