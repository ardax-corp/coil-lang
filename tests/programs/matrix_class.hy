// A matrix class over a flat `Vec<int>`: multiply, transpose, trace and a hand-written `Eq` instance.
// @output [[19, 22], [43, 50]] [[1, 3], [2, 4]] 5 true\n
use io::sync::println;
use string::format;

class Mat {
    pub rows: int,
    pub cols: int,
    pub data: Vec<int>,
}

impl Mat {
    pub static fn from(Vec<Vec<int>> rows) -> Mat {
        let d: Vec<int> = Vec::new();
        for r in rows {
            for x in r {
                d.push(x);
            }
        }
        return new Mat(rows.len(), rows[0].len(), d);
    }
    pub fn get(int r, int c) -> int {
        return self.data[r * self.cols + c];
    }
    pub fn set(int r, int c, int v) {
        self.data[r * self.cols + c] = v;
    }
    pub fn mul(Mat o) -> Mat {
        let out = Mat::zeros(self.rows, o.cols);
        for i in 0..self.rows {
            for j in 0..o.cols {
                let s = 0;
                for k in 0..self.cols {
                    s += self.get(i, k) * o.get(k, j);
                }
                out.set(i, j, s);
            }
        }
        return out;
    }
    pub static fn zeros(int r, int c) -> Mat {
        let d: Vec<int> = Vec::new();
        for _ in 0..(r * c) {
            d.push(0);
        }
        return new Mat(r, c, d);
    }
    pub fn transpose() -> Mat {
        let out = Mat::zeros(self.cols, self.rows);
        for i in 0..self.rows {
            for j in 0..self.cols {
                out.set(j, i, self.get(i, j));
            }
        }
        return out;
    }
    pub fn to_rows() -> Vec<Vec<int>> {
        let out: Vec<Vec<int>> = Vec::new();
        for i in 0..self.rows {
            let r: Vec<int> = Vec::new();
            for j in 0..self.cols {
                r.push(self.get(i, j));
            }
            out.push(r);
        }
        return out;
    }
    pub fn trace() -> int {
        let t = 0;
        for i in 0..self.rows {
            t += self.get(i, i);
        }
        return t;
    }
}

impl Eq for Mat {
    fn eq(Mat a, Mat b) -> bool {
        return a.to_rows() == b.to_rows();
    }
    fn ne(Mat a, Mat b) -> bool {
        return !(a == b);
    }
}

fn main() {
    let a = Mat::from(Vec::from([Vec::from([1, 2]), Vec::from([3, 4])]));
    let b = Mat::from(Vec::from([Vec::from([5, 6]), Vec::from([7, 8])]));
    let _ = println(
        format(
            "%v %v %i %z",
            a.mul(b).to_rows(),
            a.transpose().to_rows(),
            a.trace(),
            a.transpose().transpose() == a,
        ),
    );
}
