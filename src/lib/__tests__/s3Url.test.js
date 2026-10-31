import { describe, expect, it } from 'vitest';
import { parseS3Url } from '../s3Url';

describe('conservative S3 URL inference', () => {
  it.each([
    ['https://peerdav.s3.cn-east-1.qiniucs.com', 'https://s3.cn-east-1.qiniucs.com', 'peerdav', 'cn-east-1'],
    ['https://my.bucket.s3.eu-west-1.amazonaws.com/', 'https://s3.eu-west-1.amazonaws.com', 'my.bucket', 'eu-west-1'],
    ['https://s3.us-west-2.amazonaws.com/my-bucket', 'https://s3.us-west-2.amazonaws.com', 'my-bucket', 'us-west-2'],
    ['https://my-bucket.s3.amazonaws.com', 'https://s3.amazonaws.com', 'my-bucket', 'us-east-1'],
    ['https://my-bucket.s3.cn-north-1.amazonaws.com.cn', 'https://s3.cn-north-1.amazonaws.com.cn', 'my-bucket', 'cn-north-1'],
    ['https://s3.cn-east-1.qiniucs.com', 'https://s3.cn-east-1.qiniucs.com', '', 'cn-east-1'],
  ])('normalizes %s without a double bucket', (url, endpoint, bucket, region) => {
    expect(parseS3Url(url)).toEqual({ endpoint, bucket, region, known: true });
  });
  it.each(['https://custom.example/bucket', 'https://bucket.s3.eu-west-1.amazonaws.com.evil.test', 'https://bucket.s3.cn-east-1.qiniucs.com/path', 'https://s3.amazonaws.com/bucket/nested', 'https://user:pass@s3.amazonaws.com', 'https://s3.amazonaws.com?bucket=test', 'ftp://s3.amazonaws.com', 'invalid', 'https://localhost:9000'])('does not guess %s', (url) => {
    expect(parseS3Url(url)).toEqual({ endpoint: url, bucket: '', region: '', known: false });
  });
});
