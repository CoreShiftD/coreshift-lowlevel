use coreshift_lowlevel::spawn::SpawnOptions;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Spawning 'ls -l'...");

    let output = SpawnOptions::builder(vec!["ls".to_string(), "-l".to_string()])
        .capture_stdout()
        .timeout_ms(5000)
        .build()?
        .run()?;

    println!("Exit status: {:?}", output.status);
    println!("Output length: {} bytes", output.stdout.len());
    println!("--- STDOUT ---");
    println!("{}", String::from_utf8_lossy(&output.stdout));
    println!("--------------");

    Ok(())
}
