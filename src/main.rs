
use io_uring::{opcode, squeue, types, IoUring, Probe};
use std::fs::{self, OpenOptions};
use std::os::unix::io::AsRawFd;

fn main() -> std::io::Result<()> {
    // 1. Initialize a real kernel io_uring instance (16-entry ring)
    let mut ring = IoUring::new(16)?;
    println!("[OK] io_uring_setup syscall succeeded!");

    // 2. Probe the Linux kernel to verify supported async opcodes
    let mut probe = Probe::new();
    ring.submitter().register_probe(&mut probe)?;
    println!(
        "[OK] Kernel opcode support -> Read: {}, Write: {}, Fsync: {}, Readv: {}",
        probe.is_supported(opcode::Read::CODE),
        probe.is_supported(opcode::Write::CODE),
        probe.is_supported(opcode::Fsync::CODE),
        probe.is_supported(opcode::Readv::CODE),
    );

    // 3. Open a test file on disk
    let path = "uring_verify.bin";
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    let fd = types::Fd(file.as_raw_fd());

    // 4. Submit 3 batched async writes at different offsets in ONE syscall!
    let chunks: [&[u8]; 3] = [b"CHUNK_0|", b"CHUNK_1|", b"CHUNK_2!"];
    unsafe {
        let mut sq = ring.submission();
        for (i, chunk) in chunks.iter().enumerate() {
            let offset = (i * 8) as u64;
            let write_op = opcode::Write::new(fd, chunk.as_ptr(), chunk.len() as _)
                .offset(offset)
                .build()
                .user_data(100 + i as u64);
            sq.push(&write_op).expect("SQ full");
        }
    }

    // Single syscall submits all 3 writes and waits for all 3 completions
    let submitted = ring.submit_and_wait(3)?;
    println!("[OK] Submitted {} batched async writes in 1 syscall", submitted);

    for cqe in ring.completion() {
        assert!(cqe.result() == 8, "Write failed with {}", cqe.result());
        println!("  -> CQE user_data={} wrote {} bytes", cqe.user_data(), cqe.result());
    }

    // 5. Submit linked async Read -> Fsync (kernel executes them in order without user-space wakeups)
    let mut read_buf = vec![0u8; 24];
    let read_op = opcode::Read::new(fd, read_buf.as_mut_ptr(), read_buf.len() as _)
        .offset(0)
        .build()
        .flags(squeue::Flags::IO_LINK)
        .user_data(200);

    let fsync_op = opcode::Fsync::new(fd).build().user_data(201);

    unsafe {
        let mut sq = ring.submission();
        sq.push(&read_op).expect("SQ full");
        sq.push(&fsync_op).expect("SQ full");
    }
    ring.submit_and_wait(2)?;

    for cqe in ring.completion() {
        assert!(cqe.result() >= 0, "Linked op failed: {}", cqe.result());
        println!("  -> Linked CQE user_data={} result={}", cqe.user_data(), cqe.result());
    }

    println!(
        "[SUCCESS] Read back from NVMe via io_uring: {:?}",
        String::from_utf8_lossy(&read_buf)
    );

    drop(file);
    let _ = fs::remove_file(path);
    Ok(())
}
