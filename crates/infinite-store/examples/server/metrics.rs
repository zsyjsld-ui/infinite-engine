pub fn stats(values:&[f64])->String{
    if values.is_empty(){return "null".into();}
    let mut sorted=values.to_vec();sorted.sort_by(f64::total_cmp);
    let percentile=|n:usize|sorted[(sorted.len()*n).div_ceil(100)-1];
    format!("{{\"meanMs\":{},\"p50Ms\":{},\"p95Ms\":{},\"p99Ms\":{},\"maxMs\":{}}}",
        sorted.iter().sum::<f64>()/sorted.len()as f64,percentile(50),percentile(95),percentile(99),percentile(100))
}
