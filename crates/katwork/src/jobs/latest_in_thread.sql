SELECT j.id,to_jsonb(j.status) AS status,
    (SELECT t.kat_id FROM tasks t WHERE t.workspace_id=j.workspace_id AND t.job_id=j.id
     ORDER BY t.created_at DESC,t.id DESC LIMIT 1) AS kat_id
FROM jobs j WHERE j.workspace_id=$1 AND j.thread_id=$2
AND (j.status IS DISTINCT FROM 'stopped' OR EXISTS (
    SELECT 1 FROM runs started WHERE started.workspace_id=j.workspace_id AND started.job_id=j.id
))
ORDER BY j.id DESC LIMIT 1
