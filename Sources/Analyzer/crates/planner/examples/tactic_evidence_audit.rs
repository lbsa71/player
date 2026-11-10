use mixlib_catalog::*;
use mixlib_planner::*;
use mixlib_types::*;
fn main() -> Result<()> {
 let path=std::env::args().nth(1).expect("render report path");
 let report:serde_json::Value=serde_json::from_slice(&std::fs::read(path)?)?;
 let plan:MixPlan=serde_json::from_value(report["plan"].clone())?;
 let store=Store::open(&default_library_directory()?)?;
 let snapshot=store.read_snapshot();let cancel=Cancellation::default();let mut cache=CandidateEvidenceCache::default();
 let request=StitchRequest {from:&plan.placements[0].track_id,to:&plan.placements[1].track_id,overlap_beats:8,context_beats:8,outgoing_after_frame:0};
 let mut out=vec![];
 for p in tactic_stitch_candidates(&snapshot,&request,&plan.policy,24,&cancel)? {
  let marker=p.header.extensions.get("tactic_cue_candidate").cloned().unwrap_or_default();
  if marker.get("overlap_beats").and_then(|v|v.as_u64())!=Some(32) {continue;}
  match cache.candidate_facts(&snapshot,&p,0,&cancel) {
   Ok(facts)=>out.push(serde_json::json!({"cue":marker,"plan":p,"facts":facts})),
   Err(e)=>out.push(serde_json::json!({"cue":marker,"error":e.to_string()}))
  }
 }
 snapshot.verify_read_snapshot(&cancel)?;
 println!("{}",serde_json::to_string(&out)?);Ok(())
}
