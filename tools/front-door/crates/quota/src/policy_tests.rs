use super::*;
use crate::{display::*,time::*};
use jiff::{Timestamp,tz::TimeZone};
use serde::de::DeserializeOwned;
use serde_json::{Value,json};
use std::{io::Write,process::{Command,Stdio},path::Path};

const NOW: &str = "2026-10-23T06:00:00Z";
fn now() -> Timestamp { NOW.parse().unwrap() }
fn at(seconds: f64) -> String { iso(add_seconds(now(),seconds).unwrap()) }
fn window(name: &str,used: f64,minutes: Option<f64>,reset: Option<f64>) -> Window {
    Window {name:name.into(),used_percent:used,window_minutes:minutes,reset_at:reset.map(at),..Default::default()}
}
fn snapshot(provider: &str,label: &str,windows: Vec<Window>) -> Snapshot {
    Snapshot{schema:SCHEMA.into(),provider:provider.into(),account:label.into(),host:Some("fixture-host".into()),active:Some(false),observed_at:Some(json!(NOW)),status:Some("ok".into()),windows,..Default::default()}
}
fn context() -> Context { Context { host:"fixture-host".into(), enabled:true,providers:vec!["agy".into(),"claude".into(),"codex".into(),"cursor".into(),"opencode".into()],timezone:Some(TimeZone::UTC),..Default::default() } }
fn python_available() -> bool {
    match Command::new("python3").arg("--version").output() { Ok(result)=>{assert!(result.status.success());true}, Err(e) if e.kind()==std::io::ErrorKind::NotFound => { eprintln!("SKIP differential oracle: python3 is absent");false }, Err(e)=>panic!("python3: {e}") }
}
fn oracle<T: DeserializeOwned>(request: Value) -> T {
    let mut child = Command::new("python3").arg("-B").arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tests/oracle.py")).env("TZ","UTC").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(serde_json::to_string(&request).unwrap().as_bytes()).unwrap();
    let output=child.wait_with_output().unwrap();
    assert!(output.status.success(),"oracle: {}",String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}
/// Python supplies only dependencies outside this batch. Rust owns all display decisions.
struct OraclePolicy { context: Context }
impl OraclePolicy {
    fn call<T:DeserializeOwned>(&self, operation: &str,args: Value) -> T {
        oracle(json!({"operation":operation,"args":args,"now":NOW,"context":{"host":self.context.host,"settings":self.context.settings,"providers":self.context.providers}}))
    }
}
impl QuotaPolicy for OraclePolicy {
    fn pace(&self,w: &Window,_:Timestamp)->Pace { self.call("pace",json!([w])) }
    fn binding<'a>(&self,rows: &[&'a DisplayWindow])->Option<&'a DisplayWindow> { let index:Option<usize>=self.call("binding",json!([rows]));index.map(|i| rows[i]) }
    fn reset_passed(&self,w:&Window,_:Timestamp)->bool { self.call("reset_passed",json!([w])) }
    fn sets_state(&self,w:&Window)->bool { self.call("sets_state",json!([w])) }
    fn allowance_spent(&self,s:&Snapshot,w:Option<&DisplayWindow>)->bool { self.call("allowance_spent",json!([s,w])) }
    fn credit_draw(&self,s:&Snapshot,spent:bool)->Option<UsageCredit> { self.call("credit_draw",json!([s,spent])) }
    fn burn_rate(&self,s:&Snapshot,c:&UsageCredit)->Option<f64> { self.call("burn_rate",json!([s,c])) }
    fn credit_advice(&self,w:&[DisplayWindow],c:Option<&BankedCredits>,_:Timestamp)->CreditAdvice { self.call("credit_advice",json!([w,c])) }
    fn normalise_usage_credits(&self,c:&[UsageCredit])->Vec<UsageCredit> { self.call::<Option<Vec<UsageCredit>>>("normalise_usage_credits",json!([c])).unwrap_or_default() }
    fn normalise_resets(&self,r:Option<&Resets>)->Option<Resets> { self.call("normalise_resets",json!([r])) }
    fn window_pool(&self,p:&str,n:&str)->Option<String> { if p=="agy" { n.split_once('.').map(|p|p.0.into()) } else { None } }
    fn fingerprinted(&self,p:&str,l:&str)->bool { p=="agy" && l.starts_with("google-") }
    fn freshest(&self,v:&[Snapshot])->Vec<Snapshot> { self.call("freshest",json!([v])) }
}
fn compare_report(values: &[Snapshot], context: &Context) -> Report {
    let policy=OraclePolicy{context:context.clone()};
    let actual=report(&policy,context,values,now()).unwrap();
    let expected:Value=oracle(json!({"operation":"report","now":NOW,"values":values,"context":{"host":context.host,"enabled":context.enabled,"providers":context.providers,"settings":context.settings,"entries":context.entries,"activity":context.activity,"refreshing":context.refreshing,"banked":context.banked.iter().map(|((p,a,h),c)| json!([p,a,h,c])).collect::<Vec<_>>()}}));
    assert_eq!(serde_json::to_value(&actual).unwrap(),expected);
    actual
}
#[test]
fn vocabulary_and_length_policy() {
    assert_eq!(provider_name("agy"),"Antigravity");assert_eq!(provider_name("opencode"),"OpenCode");assert_eq!(provider_name("big_model"),"Big Model");
    assert_eq!(plan_label(Some("pro_student")).as_deref(),Some("Pro (Student)"));
    assert_eq!(plan_label(Some("Max")).as_deref(),Some("Max"));assert_eq!(plan_label(None),None);
    let labels=window_labels(&context());
    for (minutes,name,label) in [(Some(300.0),"five_hour","5-hour"),(Some(1440.0),"daily","Daily"),(Some(4320.0),"odd","3-day (Odd)"),(Some(90.0),"burst","90-minute (Burst)"),(Some(44640.0),"billing_cycle","Monthly"),(Some(43200.0),"thirty_day","Monthly"),(Some(10080.0),"seven_day_opus","Weekly (Opus)"),(Some(10080.0),"weekly_scoped:fable","Weekly (Fable)"),(Some(10080.0),"weekly_all:2","Weekly"),(None,"odd_window_name","Odd window name"),(None,"iguana_necktie","Cloud session credits")] {
        assert_eq!(window_label(&window(name,0.0,minutes,None),&labels),label);
    }
    let mut pooled=window("claude.5h",0.0,Some(300.0),None);pooled.pool=Some("claude".into());assert_eq!(window_label(&pooled,&labels),"5-hour");
    assert_eq!(length_label(Some(90.5)),Some("90-minute".into()));
    assert_eq!(amount(1234.5,Some("USD")),"$1,234");assert_eq!(amount(1235.5,Some("AUD")),"1,236 AUD");
    assert_eq!(duration(172800.0+3600.0),"2d 1h");assert_eq!(used_text(&window("w",12.345678,None,None)),"12.3457%");
}
#[test]
fn invalid_alerts_fall_back_and_labels_are_bounded() {
    for value in [json!(null),json!(true),json!(0),json!(101),json!("nan")] { let mut c=context();c.settings=json!({"display":{"short_window_alert_percent":value}});assert_eq!(alert_percent(&c),90.0); }
    let mut c=context();c.settings=json!({"display":{"short_window_alert_percent":"80"}});assert_eq!(alert_percent(&c),80.0);
    c.entries=json!({"claude":{"work":{"display_name":" x "},"long":{"display_name":"é".repeat(80)}}});
    // These paths need no provider policy calls.
    let p=OraclePolicy{context:c.clone()};assert_eq!(display_name(&p,&c,"claude","work"),Some("x".into()));assert_eq!(display_name(&p,&c,"claude","long").unwrap().chars().count(),64);
}
#[test]
fn display_report_parity_thresholds_staleness_restatement_and_pools() {
    if !python_available(){return;}
    for (weekly,short,expected) in [(30.0,89.0,"seven_day"),(30.0,90.0,"five_hour"),(95.0,90.0,"five_hour"),(95.0,50.0,"seven_day"),(99.0,92.0,"five_hour"),(30.0,100.0,"five_hour")] {
        let s=snapshot("claude","work",vec![window("five_hour",short,Some(300.0),Some(3600.0)),window("seven_day",weekly,Some(10080.0),Some(172800.0))]);
        let r=compare_report(&[s],&context());assert_eq!(r.accounts[0].display_window.as_deref(),Some(expected));
    }
    let mut s=snapshot("claude","work",vec![window("five_hour",100.0,Some(300.0),Some(-3600.0)),window("seven_day",20.0,Some(10080.0),Some(172800.0)),window("weekly_all:2",20.0,Some(10080.0),Some(172850.0))]);s.observed_at=Some(json!(at(-7200.0)));
    let r=compare_report(&[s],&context());assert_eq!(r.accounts[0].status.as_deref(),Some("stale"));assert_eq!(r.accounts[0].windows.len(),2);assert_eq!(r.accounts[0].back_at,None);
    for (gemini,claude,expected) in [(100.0,6.0,"claude.weekly"),(100.0,100.0,"gemini.weekly")] {
        let s=snapshot("agy","google-000000000001",vec![window("gemini.weekly",gemini,Some(10080.0),Some(172800.0)),window("claude.weekly",claude,Some(10080.0),Some(518400.0)),window("claude.5h",12.0,Some(300.0),Some(3600.0))]);let r=compare_report(&[s],&context());assert_eq!(r.accounts[0].display_window.as_deref(),Some(expected));
    }
}
#[test]
fn display_report_parity_refusals_and_signed_out_history() {
    if !python_available(){return;}
    for (reached,all_reset,spend_cap,unreported,exhausted) in [(Some("primary"),false,false,false,false),(Some("primary"),false,false,true,false),(Some("secondary"),false,false,false,true),(Some("unreported_window"),true,false,true,true),(None,false,false,false,true),(None,true,false,false,false),(None,true,false,true,true),(Some("primary"),true,true,false,true),(Some("workspace_member_credits_depleted"),true,false,false,true)] {
        let mut s=snapshot("codex","default",vec![window("primary",60.0,Some(10080.0),Some(-3600.0)),window("secondary",20.0,Some(300.0),Some(if all_reset{-3600.0}else{7200.0}))]);s.observed_at=Some(json!(at(-7200.0)));s.allowance=Some(Allowance{ordinary_usage_allowed:Some(false),spend_control_reached:Some(spend_cap),limit_reached_type:reached.map(Into::into)});if unreported{s.unreported.push("missing".into());}
        let r=compare_report(&[s],&context());assert_eq!(r.accounts[0].state=="exhausted",exhausted);
    }
    let mut s=snapshot("agy","google-000000000001",vec![window("gemini.weekly",100.0,Some(10080.0),Some(-172800.0)),window("claude.weekly",100.0,Some(10080.0),Some(-86400.0))]);s.status=Some("signed_out".into());s.observed_at=Some(json!(at(-604800.0)));
    let r=compare_report(&[s],&context());assert_eq!(r.accounts[0].state,"signed_out");assert!(r.accounts[0].display_window.is_none());assert!(r.placement_preference.is_empty());assert!(r.accounts[0].back_at.is_some());
}
#[test]
fn display_report_parity_headlines_priority_and_reserves() {
    if !python_available(){return;}
    let mut a=snapshot("claude","work",vec![window("seven_day",9.0,Some(10080.0),Some(518400.0))]);a.priority=Some(5);
    let mut b=snapshot("codex","default",vec![window("primary",83.0,Some(10080.0),Some(432000.0))]);b.priority=Some(1);
    let mut spent=snapshot("claude","away",vec![window("seven_day",100.0,Some(10080.0),Some(259200.0))]);spent.status=Some("signed_out".into());
    for (mode,expected) in [("recent",Some("claude/work")),("worst",Some("codex/default")),("none",None),("claude/away",Some("claude/away")),("missing/entry",Some("claude/work")),("bogus",Some("claude/work"))] {
        let mut c=context();c.activity=BTreeMap::from([("claude/work".into(),200.0),("codex/default".into(),100.0),("claude/away".into(),300.0)]);c.settings=json!({"menubar":{"headline":mode}});let r=compare_report(&[a.clone(),b.clone(),spent.clone()],&c);assert_eq!(r.headline.account.as_deref(),expected);
    }
    for drawing in [false,true] {
        let mut s=snapshot("codex","default",vec![window("primary",33.0,Some(10080.0),Some(159.0*3600.0))]);
        s.usage_credits=vec![UsageCredit{balance:Some(26000.0),unit:Some("credits".into()),..Default::default()}];s.credits=Some(BankedCredits{reset_credits:1,source:Some("provider".into())});
        if drawing { s.allowance=Some(Allowance{ordinary_usage_allowed:Some(false),..Default::default()});s.extra.insert("credit_anchor".into(),json!({"balance":27000,"observed_at":at(-3600.0)})); }
        let r=compare_report(&[s],&context());assert!(r.accounts[0].summary.contains("banked reset") || drawing);assert_eq!(r.accounts[0].drawing_credits, drawing.then_some(true));
    }
}
