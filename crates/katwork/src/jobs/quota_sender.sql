SELECT count(*) FROM jobs j JOIN tasks t ON t.job_id=j.id AND t.workspace_id=j.workspace_id
WHERE j.workspace_id=$1 AND j.requested_by=$2
  AND j.created_at>statement_timestamp()-interval '60 seconds' AND t.kat_id=$3
