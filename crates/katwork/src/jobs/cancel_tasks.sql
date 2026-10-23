SELECT id FROM tasks WHERE workspace_id=$1 AND job_id=$2 ORDER BY id FOR NO KEY UPDATE
