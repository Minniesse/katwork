-- Bound stopped-job run lookups while intake holds the thread lock.
CREATE INDEX runs_job_id ON runs (job_id);
